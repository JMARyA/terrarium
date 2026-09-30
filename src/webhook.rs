//! Per-workspace webhooks.
//!
//! Every delivery is signed so receivers can verify it came from this server:
//!
//! | Header                  | Value                                              |
//! | ----------------------- | -------------------------------------------------- |
//! | `X-Terrarium-Event`     | event name, e.g. `state.push`                      |
//! | `X-Terrarium-Delivery`  | UUID, identical across retries (for idempotency)   |
//! | `X-Terrarium-Timestamp` | Unix seconds at send time (for replay protection)  |
//! | `X-Terrarium-Signature` | `sha256=` + hex HMAC-SHA256 of `"{timestamp}.{body}"` |
//!
//! Outbound requests are constrained: short timeouts, no redirects, and a DNS
//! resolver that refuses internal addresses per `TERRARIUM_WEBHOOK_NETWORKS`,
//! so a registered URL can't turn the server into a probe of its own network.
//! See docs/webhooks.md.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::auth::AuthUser;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use hmac::{Hmac, KeyInit as _, Mac as _};
use reqwest::Client;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::RwLock;

use crate::AppState;

/// Every event Terrarium fires. Registration rejects anything else: a typo'd
/// event would store fine and silently never fire.
pub const EVENTS: &[&str] = &[
    "state.push",
    "state.delete",
    "state.archive",
    "state.unarchive",
    "lock.acquire",
    "lock.release",
    "lock.expire",
];

const MAX_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub workspace: String,
    pub url: String,
    /// Events to subscribe to. Empty = all events for this workspace.
    pub events: Vec<String>,
    /// HMAC-SHA256 signing key. Handed out only when the hook is created or
    /// its secret rotated; never included in listings.
    #[serde(default)]
    pub secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// A webhook as the API returns it. `secret` is present only in the response
/// that created the hook or rotated its secret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookInfo {
    pub id: String,
    pub workspace: String,
    pub url: String,
    pub events: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

impl Webhook {
    fn info(&self, reveal_secret: bool) -> WebhookInfo {
        WebhookInfo {
            id: self.id.clone(),
            workspace: self.workspace.clone(),
            url: self.url.clone(),
            events: self.events.clone(),
            secret: reveal_secret.then(|| self.secret.clone()),
            created_by: self.created_by.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct WebhookPayload {
    pub event: String,
    pub workspace: String,
    pub version: Option<u32>,
    pub user: String,
    pub timestamp: String,
}

// ── Network policy ───────────────────────────────────────────────────────────

/// Which destinations webhooks may reach (`TERRARIUM_WEBHOOK_NETWORKS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Public addresses only.
    Public,
    /// Public and private (RFC 1918, CGNAT, IPv6 ULA) addresses. The default:
    /// self-hosted receivers usually live on a private network.
    Private,
    /// No restriction, including loopback and cloud metadata endpoints.
    Any,
}

impl NetworkPolicy {
    pub fn from_env() -> Self {
        match std::env::var("TERRARIUM_WEBHOOK_NETWORKS").as_deref().map(str::trim) {
            Err(_) | Ok("private") => Self::Private,
            Ok("public") => Self::Public,
            Ok("any") => Self::Any,
            Ok(other) => {
                tracing::warn!("Invalid TERRARIUM_WEBHOOK_NETWORKS {other:?} (expected public, private or any), using private");
                Self::Private
            }
        }
    }

    pub fn allows(self, ip: IpAddr) -> bool {
        matches!(
            (self, classify(ip)),
            (Self::Any, _) | (_, IpClass::Public) | (Self::Private, IpClass::Private)
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
enum IpClass {
    Public,
    Private,
    /// Loopback, link-local (incl. the 169.254.169.254 metadata endpoint),
    /// unspecified, multicast and broadcast: never a legitimate receiver.
    Internal,
}

fn classify(ip: IpAddr) -> IpClass {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_multicast()
                || v4.is_broadcast() || a == 0
            {
                IpClass::Internal
            } else if v4.is_private() || (a == 100 && (64..128).contains(&b)) {
                IpClass::Private
            } else {
                IpClass::Public
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return classify(IpAddr::V4(v4));
            }
            // fd00:ec2::254 is AWS's IPv6 instance metadata endpoint.
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || v6.is_unicast_link_local()
                || v6 == std::net::Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
            {
                IpClass::Internal
            } else if v6.is_unique_local() {
                IpClass::Private
            } else {
                IpClass::Public
            }
        }
    }
}

/// Resolver that drops addresses the policy forbids. Filtering at resolution
/// time (rather than only when a hook is registered) also defeats DNS
/// rebinding: the address that is checked is the address that is dialed.
struct GuardedResolver {
    policy: NetworkPolicy,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.policy;
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| policy.allows(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} resolves only to addresses blocked by TERRARIUM_WEBHOOK_NETWORKS").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// Validate a webhook URL's shape and, for an IP-literal host (which never
/// reaches the resolver), its address.
fn check_url(url: &str, policy: NetworkPolicy) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("webhook URL must be http or https".into());
    }
    let Some(host) = parsed.host_str() else {
        return Err("webhook URL has no host".into());
    };
    let ip = host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok();
    if ip.is_some_and(|ip| !policy.allows(ip)) {
        return Err("webhook URL points at an address blocked by TERRARIUM_WEBHOOK_NETWORKS".into());
    }
    Ok(parsed)
}

/// `scheme://host[:port]` — webhook paths and queries often embed tokens
/// (Slack, Discord, …), so they never go into logs.
fn redact(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(port) => format!("{}://{host}:{port}", u.scheme()),
                None => format!("{}://{host}", u.scheme()),
            })
        })
        .unwrap_or_else(|| "<invalid url>".to_string())
}

// ── Signing ──────────────────────────────────────────────────────────────────

fn generate_secret() -> String {
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).expect("OS random number generator unavailable");
    format!("whsec_{}", hex(&key))
}

/// `sha256=` + hex HMAC-SHA256 over `"{timestamp}.{body}"`.
pub fn sign(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Store ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebhookStore {
    pub hooks: Arc<RwLock<Vec<Webhook>>>,
    pub path: PathBuf,
    client: Client,
    policy: NetworkPolicy,
}

impl WebhookStore {
    pub fn new(path: PathBuf, policy: NetworkPolicy) -> Self {
        let mut hooks: Vec<Webhook> = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Hooks registered before signing existed get a secret now, so every
        // delivery is signed. Their owners fetch it with `rotate-secret`.
        let mut backfilled = 0;
        for hook in hooks.iter_mut().filter(|h| h.secret.is_empty()) {
            hook.secret = generate_secret();
            backfilled += 1;
        }
        if backfilled > 0 {
            tracing::info!("🪝 Generated signing secrets for {backfilled} existing webhook(s); rotate them to obtain the secret");
            if let Err(e) = write_hooks(&path, &hooks) {
                tracing::error!("💥 Failed to persist webhooks: {e}");
            }
        }

        let mut client = Client::builder()
            .user_agent(concat!("terrarium-webhook/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none());
        if policy != NetworkPolicy::Any {
            // A proxy resolves the target itself, bypassing the guard.
            client = client.no_proxy().dns_resolver(Arc::new(GuardedResolver { policy }));
        }

        Self {
            hooks: Arc::new(RwLock::new(hooks)),
            path,
            client: client.build().expect("failed to build webhook HTTP client"),
            policy,
        }
    }

    pub async fn add(&self, webhook: Webhook) -> std::io::Result<()> {
        let mut hooks = self.hooks.write().await;
        hooks.push(webhook);
        if let Err(e) = write_hooks(&self.path, &hooks) {
            hooks.pop();
            return Err(e);
        }
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> std::io::Result<bool> {
        let mut hooks = self.hooks.write().await;
        let before = hooks.len();
        hooks.retain(|h| h.id != id);
        if hooks.len() == before {
            return Ok(false);
        }
        write_hooks(&self.path, &hooks)?;
        Ok(true)
    }

    /// Replace a hook's signing secret, returning the updated hook.
    pub async fn rotate_secret(&self, id: &str) -> std::io::Result<Option<Webhook>> {
        let mut hooks = self.hooks.write().await;
        let Some(hook) = hooks.iter_mut().find(|h| h.id == id) else {
            return Ok(None);
        };
        hook.secret = generate_secret();
        let rotated = hook.clone();
        write_hooks(&self.path, &hooks)?;
        Ok(Some(rotated))
    }

    pub async fn list_for(&self, workspace: &str) -> Vec<Webhook> {
        self.hooks
            .read()
            .await
            .iter()
            .filter(|h| h.workspace == workspace)
            .cloned()
            .collect()
    }

    /// Fire all matching webhooks for an event in background tasks (non-blocking).
    pub async fn fire(&self, event: &str, workspace: &str, version: Option<u32>, user: &str) {
        let hooks = self.hooks.read().await;
        let matching: Vec<Webhook> = hooks
            .iter()
            .filter(|h| {
                h.workspace == workspace
                    && (h.events.is_empty() || h.events.iter().any(|e| e == event))
            })
            .cloned()
            .collect();
        drop(hooks);

        if matching.is_empty() {
            return;
        }

        let payload = Arc::new(WebhookPayload {
            event: event.to_string(),
            workspace: workspace.to_string(),
            version,
            user: user.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });

        for hook in matching {
            let client = self.client.clone();
            let payload = Arc::clone(&payload);
            let policy = self.policy;
            tokio::spawn(async move {
                deliver(&client, &hook, &payload, policy).await;
            });
        }
    }
}

/// Webhook secrets live in this file, so it is written owner-only.
fn write_hooks(path: &std::path::Path, hooks: &[Webhook]) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(hooks).map_err(std::io::Error::other)?;
    crate::state::atomic_write_private(path, &json)
}

/// Only transient failures are worth retrying; a 4xx will fail the same way.
fn retryable(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

async fn deliver(client: &Client, hook: &Webhook, payload: &WebhookPayload, policy: NetworkPolicy) {
    let target = redact(&hook.url);
    let record = |result: &'static str, elapsed: Option<Duration>| {
        metrics::counter!("terrarium_webhook_deliveries_total", "workspace" => payload.workspace.clone(), "event" => payload.event.clone(), "result" => result).increment(1);
        if let Some(elapsed) = elapsed {
            metrics::histogram!("terrarium_webhook_delivery_duration_seconds", "workspace" => payload.workspace.clone(), "event" => payload.event.clone(), "result" => result).record(elapsed.as_secs_f64());
        }
    };

    // Re-checked on every delivery: the hook may predate the current policy.
    if let Err(e) = check_url(&hook.url, policy) {
        record("blocked", None);
        tracing::warn!("🪝 Webhook {} to {target} not delivered: {e}", hook.id);
        return;
    }
    let body = match serde_json::to_vec(payload) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!("🪝 Failed to serialize webhook payload: {e}");
            return;
        }
    };
    let delivery = uuid::Uuid::new_v4().to_string();

    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            metrics::counter!("terrarium_webhook_retries_total", "workspace" => payload.workspace.clone(), "event" => payload.event.clone())
                .increment(1);
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
        let timestamp = chrono::Utc::now().timestamp();
        let started = std::time::Instant::now();
        let sent = client
            .post(&hook.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("X-Terrarium-Event", &payload.event)
            .header("X-Terrarium-Delivery", &delivery)
            .header("X-Terrarium-Timestamp", timestamp.to_string())
            .header("X-Terrarium-Signature", sign(&hook.secret, timestamp, &body))
            .body(body.clone())
            .send()
            .await;
        match sent {
            Ok(resp) if resp.status().is_success() => {
                record("ok", Some(started.elapsed()));
                return;
            }
            Ok(resp) => {
                record("http_error", Some(started.elapsed()));
                let status = resp.status();
                if !retryable(status) {
                    tracing::warn!("🪝 Webhook {target} returned {status} (attempt {attempt}), not retrying");
                    return;
                }
                tracing::warn!("🪝 Webhook {target} returned {status} (attempt {attempt})");
            }
            Err(e) => {
                record("error", Some(started.elapsed()));
                // `without_url`: the full URL may carry a token.
                tracing::warn!("🪝 Webhook {target} error: {} (attempt {attempt})", e.without_url());
            }
        }
    }
    tracing::error!("🪝 Webhook {target} failed after {MAX_ATTEMPTS} attempts (delivery {delivery})");
}

// ── API handlers ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddWebhookBody {
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
}

type ApiError = (StatusCode, String);

fn bad_request(msg: impl Into<String>) -> ApiError {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn storage_error(e: std::io::Error) -> ApiError {
    tracing::error!("💥 Failed to persist webhooks: {e}");
    (StatusCode::INTERNAL_SERVER_ERROR, "failed to persist webhooks".into())
}

/// POST /webhooks/{*workspace} — register a webhook for a workspace.
/// The response is the only time the signing secret is shown.
pub async fn add_webhook(
    State(app): State<AppState>,
    Path(workspace): Path<String>,
    AuthUser(user): AuthUser,
    Json(body): Json<AddWebhookBody>,
) -> Result<Json<WebhookInfo>, ApiError> {
    crate::state::validate_name(&workspace).map_err(|_| bad_request("invalid workspace name"))?;
    if let Some(unknown) = body.events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        return Err(bad_request(format!("unknown event {unknown:?}; supported: {}", EVENTS.join(", "))));
    }
    let url = check_url(&body.url, app.webhooks.policy).map_err(bad_request)?;

    // Catch hostnames that only resolve internally now, rather than at the
    // first delivery. A name that doesn't resolve yet is accepted: the
    // receiver may simply not be deployed. Delivery re-checks regardless.
    if let Some(host) = url.host_str()
        && app.webhooks.policy != NetworkPolicy::Any
        && let Ok(addrs) = tokio::net::lookup_host((host, 0)).await
    {
        let addrs: Vec<SocketAddr> = addrs.collect();
        if !addrs.is_empty() && !addrs.iter().any(|a| app.webhooks.policy.allows(a.ip())) {
            return Err(bad_request(format!("{host} resolves only to addresses blocked by TERRARIUM_WEBHOOK_NETWORKS")));
        }
    }

    let hook = Webhook {
        id: uuid::Uuid::new_v4().to_string(),
        workspace,
        url: body.url,
        events: body.events,
        secret: generate_secret(),
        created_by: Some(user.username.clone()),
    };
    app.webhooks.add(hook.clone()).await.map_err(storage_error)?;
    tracing::info!("🪝 {} registered webhook {} on {} → {}", user.username, hook.id, hook.workspace, redact(&hook.url));
    Ok(Json(hook.info(true)))
}

/// GET /webhooks/{*workspace} — list webhooks for a workspace (without secrets)
pub async fn list_webhooks(
    State(app): State<AppState>,
    Path(workspace): Path<String>,
    _auth: AuthUser,
) -> Json<Vec<WebhookInfo>> {
    Json(app.webhooks.list_for(&workspace).await.iter().map(|h| h.info(false)).collect())
}

/// DELETE /webhooks/id/{id} — remove a webhook by ID
pub async fn remove_webhook(
    State(app): State<AppState>,
    Path(id): Path<String>,
    _auth: AuthUser,
) -> Result<StatusCode, ApiError> {
    match app.webhooks.remove(&id).await.map_err(storage_error)? {
        true => Ok(StatusCode::OK),
        false => Err((StatusCode::NOT_FOUND, "no such webhook".into())),
    }
}

/// POST /webhooks/id/{id}/rotate-secret — replace the signing secret and
/// return the new one. The old secret stops working immediately.
pub async fn rotate_secret(
    State(app): State<AppState>,
    Path(id): Path<String>,
    AuthUser(user): AuthUser,
) -> Result<Json<WebhookInfo>, ApiError> {
    match app.webhooks.rotate_secret(&id).await.map_err(storage_error)? {
        Some(hook) => {
            tracing::info!("🪝 {} rotated the secret of webhook {id}", user.username);
            Ok(Json(hook.info(true)))
        }
        None => Err((StatusCode::NOT_FOUND, "no such webhook".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("terrarium-webhook-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("webhooks.json")
    }

    #[test]
    fn signature_matches_reference_hmac() {
        // Reference computed independently with Python's hmac module.
        assert_eq!(
            sign("whsec_test", 1_700_000_000, br#"{"a":1}"#),
            "sha256=38877139021993b830af32feea6e18a8da83eb2f6e49ee50bd9e4cf4ca4d3789"
        );
    }

    #[test]
    fn secrets_are_random_and_prefixed() {
        let (a, b) = (generate_secret(), generate_secret());
        assert!(a.starts_with("whsec_") && a.len() == "whsec_".len() + 64);
        assert_ne!(a, b);
    }

    #[test]
    fn classifies_addresses() {
        for internal in ["127.0.0.1", "169.254.169.254", "0.0.0.0", "224.0.0.1", "255.255.255.255", "::1", "fe80::1", "::ffff:127.0.0.1", "fd00:ec2::254"] {
            assert_eq!(classify(ip(internal)), IpClass::Internal, "{internal}");
        }
        for private in ["10.0.0.1", "172.16.5.4", "192.168.1.1", "100.64.0.1", "fd12:3456::1", "::ffff:10.0.0.1"] {
            assert_eq!(classify(ip(private)), IpClass::Private, "{private}");
        }
        for public in ["1.1.1.1", "100.128.0.1", "2606:4700::1111"] {
            assert_eq!(classify(ip(public)), IpClass::Public, "{public}");
        }
    }

    #[test]
    fn policies_gate_address_classes() {
        let (public, private, internal) = (ip("1.1.1.1"), ip("10.0.0.1"), ip("127.0.0.1"));
        assert!(NetworkPolicy::Public.allows(public) && !NetworkPolicy::Public.allows(private) && !NetworkPolicy::Public.allows(internal));
        assert!(NetworkPolicy::Private.allows(public) && NetworkPolicy::Private.allows(private) && !NetworkPolicy::Private.allows(internal));
        assert!(NetworkPolicy::Any.allows(internal));
    }

    #[test]
    fn check_url_rejects_bad_schemes_and_blocked_literals() {
        let p = NetworkPolicy::Private;
        assert!(check_url("ftp://example.com/x", p).is_err());
        assert!(check_url("file:///etc/passwd", p).is_err());
        assert!(check_url("not a url", p).is_err());
        assert!(check_url("http://127.0.0.1:8080/", p).is_err());
        assert!(check_url("http://169.254.169.254/latest/meta-data/", p).is_err());
        assert!(check_url("http://[::ffff:127.0.0.1]/", p).is_err());
        assert!(check_url("http://10.1.2.3/hook", p).is_ok());
        assert!(check_url("https://hooks.example.com/tf", p).is_ok());
        assert!(check_url("http://10.1.2.3/hook", NetworkPolicy::Public).is_err());
        assert!(check_url("http://127.0.0.1/", NetworkPolicy::Any).is_ok());
    }

    #[test]
    fn redacts_path_and_query() {
        assert_eq!(redact("https://hooks.slack.com/services/T0/B0/secret?x=1"), "https://hooks.slack.com");
        assert_eq!(redact("http://10.0.0.1:9000/token"), "http://10.0.0.1:9000");
    }

    #[test]
    fn only_transient_statuses_are_retried() {
        assert!(retryable(reqwest::StatusCode::BAD_GATEWAY));
        assert!(retryable(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(!retryable(reqwest::StatusCode::NOT_FOUND));
        assert!(!retryable(reqwest::StatusCode::UNAUTHORIZED));
    }

    #[test]
    fn legacy_hooks_get_a_secret_and_the_file_is_private() {
        let path = tmp_path("legacy");
        std::fs::write(&path, r#"[{"id":"h1","workspace":"infra/prod","url":"https://example.com","events":[]}]"#).unwrap();

        let store = WebhookStore::new(path.clone(), NetworkPolicy::Private);
        let hooks = store.hooks.try_read().unwrap();
        assert!(hooks[0].secret.starts_with("whsec_"));

        let on_disk: Vec<Webhook> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk[0].secret, hooks[0].secret, "backfilled secret must be persisted");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn listing_never_reveals_the_secret() {
        let hook = Webhook {
            id: "h".into(),
            workspace: "w".into(),
            url: "https://example.com".into(),
            events: vec![],
            secret: "whsec_x".into(),
            created_by: None,
        };
        let listed = serde_json::to_string(&hook.info(false)).unwrap();
        assert!(!listed.contains("whsec_x") && !listed.contains("secret"));
        assert_eq!(hook.info(true).secret.as_deref(), Some("whsec_x"));
    }

    /// Spin up a receiver on loopback and capture the first request's
    /// signature headers and body.
    async fn receiver() -> (SocketAddr, tokio::sync::mpsc::Receiver<(axum::http::HeaderMap, axum::body::Bytes)>) {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send((headers, body)).await;
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (addr, rx)
    }

    fn store_with_hook(policy: NetworkPolicy, url: String) -> (WebhookStore, String, PathBuf) {
        let path = tmp_path("deliver");
        let store = WebhookStore::new(path.clone(), policy);
        let secret = generate_secret();
        store.hooks.try_write().unwrap().push(Webhook {
            id: "h".into(),
            workspace: "infra/prod".into(),
            url,
            events: vec![],
            secret: secret.clone(),
            created_by: None,
        });
        (store, secret, path)
    }

    #[tokio::test]
    async fn delivery_is_signed_and_verifiable() {
        let (addr, mut rx) = receiver().await;
        let (store, secret, path) = store_with_hook(NetworkPolicy::Any, format!("http://{addr}/hook"));

        store.fire("state.push", "infra/prod", Some(3), "alice").await;
        let (headers, body) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();

        let header = |name: &str| headers.get(name).unwrap().to_str().unwrap().to_string();
        let timestamp: i64 = header("x-terrarium-timestamp").parse().unwrap();
        assert_eq!(header("x-terrarium-event"), "state.push");
        assert!(uuid::Uuid::parse_str(&header("x-terrarium-delivery")).is_ok());
        assert_eq!(header("x-terrarium-signature"), sign(&secret, timestamp, &body));
        assert!((chrono::Utc::now().timestamp() - timestamp).abs() < 60);

        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["event"], "state.push");
        assert_eq!(payload["version"], 3);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn loopback_receivers_are_blocked_by_default() {
        let (addr, mut rx) = receiver().await;
        // Both an IP literal and a hostname that resolves to loopback.
        for url in [format!("http://{addr}/hook"), format!("http://localhost:{}/hook", addr.port())] {
            let (store, _, path) = store_with_hook(NetworkPolicy::Private, url.clone());
            store.fire("state.push", "infra/prod", None, "alice").await;
            let got = tokio::time::timeout(Duration::from_millis(1500), rx.recv()).await;
            assert!(got.is_err(), "{url} must not be delivered");
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }
}
