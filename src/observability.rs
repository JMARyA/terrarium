use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::AppState;

static PROMETHEUS: OnceLock<PrometheusHandle> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

pub fn enabled() -> bool {
    std::env::var("TERRARIUM_METRICS")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub fn init() {
    START.get_or_init(Instant::now);
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus metrics recorder");
    let _ = PROMETHEUS.set(handle);
    gauge!("terrarium_build_info", "version" => env!("CARGO_PKG_VERSION")).set(1.0);
}

pub async fn metrics(headers: HeaderMap) -> impl IntoResponse {
    match metrics_token() {
        Some(token) => {
            let ok = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h == format!("Bearer {token}"));
            if !ok {
                return (StatusCode::UNAUTHORIZED, "unauthorized\n".to_string());
            }
        }
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "metrics token is not configured\n".to_string(),
            );
        }
    }

    match PROMETHEUS.get() {
        Some(handle) => (StatusCode::OK, handle.render()),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "metrics recorder is not initialized\n".to_string(),
        ),
    }
}

fn metrics_token() -> Option<String> {
    if let Ok(path) = std::env::var("TERRARIUM_METRICS_TOKEN_FILE") {
        return std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    std::env::var("TERRARIUM_METRICS_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
}

pub async fn http_middleware(req: Request<Body>, next: Next) -> Response {
    let method = req.method().as_str().to_string();
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| scrub_path(req.uri().path()));
    if let Some(len) = content_length(req.headers()) {
        histogram!("terrarium_http_request_body_bytes", "route" => route.clone())
            .record(len as f64);
    }

    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16().to_string();
    if let Some(len) = content_length(response.headers()) {
        histogram!("terrarium_http_response_body_bytes", "route" => route.clone())
            .record(len as f64);
    }
    let elapsed = started.elapsed().as_secs_f64();

    counter!("terrarium_http_requests_total", "method" => method.clone(), "route" => route.clone(), "status" => status.clone()).increment(1);
    histogram!("terrarium_http_request_duration_seconds", "method" => method, "route" => route, "status" => status).record(elapsed);
    response
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn scrub_path(path: &str) -> String {
    // Fallback only. MatchedPath should normally provide normalized Axum routes.
    if path == "/metrics" {
        return "/metrics".into();
    }
    if path.starts_with("/state/") {
        return "/state/{*name}".into();
    }
    if path.starts_with("/lock/") {
        return "/lock/{*name}".into();
    }
    if path.starts_with("/versions/") {
        return "/versions/{*name}".into();
    }
    if path.starts_with("/archive/") {
        return "/archive/{*name}".into();
    }
    if path.starts_with("/webhooks/") {
        return "/webhooks/{*workspace}".into();
    }
    if path.starts_with("/registry/") {
        return "/registry/...".into();
    }
    path.to_string()
}

pub fn spawn_collector(app: AppState, data_dir: PathBuf) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            collect(&app, &data_dir).await;
            ticker.tick().await;
        }
    });
}

async fn collect(app: &AppState, data_dir: &Path) {
    let active_states = app.state.list(None, false);
    let archived_states = app.state.list(None, true);
    gauge!("terrarium_states_total", "archived" => "false").set(active_states.len() as f64);
    gauge!("terrarium_states_total", "archived" => "true").set(archived_states.len() as f64);
    gauge!("terrarium_state_versions_total").set(count_files(&data_dir.join("versions")) as f64);
    for name in &active_states {
        if let Some(bytes) = app.state.get(name) {
            histogram!("terrarium_state_size_bytes").record(bytes.len() as f64);
            gauge!("terrarium_state_current_bytes", "workspace" => name.clone())
                .set(bytes.len() as f64);
            if let Some((resources, instances, outputs)) = state_counts(&bytes) {
                gauge!("terrarium_tf_resources_total", "workspace" => name.clone())
                    .set(resources as f64);
                gauge!("terrarium_tf_resource_instances_total", "workspace" => name.clone())
                    .set(instances as f64);
                gauge!("terrarium_tf_outputs_total", "workspace" => name.clone())
                    .set(outputs as f64);
            }
            if let Some(serial) = state_serial(&bytes) {
                gauge!("terrarium_state_serial", "workspace" => name.clone()).set(serial as f64);
            }
        }
        histogram!("terrarium_state_versions_per_state")
            .record(app.state.list_versions(name).len() as f64);
        if let Some(ts) = latest_version_mtime(&app.state, name) {
            gauge!("terrarium_state_last_activity_timestamp_seconds", "workspace" => name.clone())
                .set(ts as f64);
        }
    }

    let current_bytes = dir_size(&data_dir.join("state"));
    let version_bytes = dir_size(&data_dir.join("versions"));
    let lock_bytes = dir_size(&data_dir.join("locks"));
    let registry_bytes = dir_size(&data_dir.join("registry"));
    gauge!("terrarium_storage_current_state_bytes").set(current_bytes as f64);
    gauge!("terrarium_storage_state_versions_bytes").set(version_bytes as f64);
    gauge!("terrarium_storage_locks_bytes").set(lock_bytes as f64);
    gauge!("terrarium_storage_registry_bytes").set(registry_bytes as f64);
    gauge!("terrarium_storage_total_bytes").set(dir_size(data_dir) as f64);

    let locks = app.locks.list();
    gauge!("terrarium_locks_active").set(locks.len() as f64);
    gauge!("terrarium_lock_max_age_seconds").set(max_lock_age(&locks) as f64);

    gauge!("terrarium_webhooks_registered").set(app.webhooks.hooks.read().await.len() as f64);

    // Per-workspace resource/instance/output totals are emitted in the loop
    // above; only the mode split and sensitive-output count stay aggregate-only.
    let (_resources, managed, data, _instances, _outputs, sensitive_outputs) =
        tf_aggregates(&app.state, &active_states);
    gauge!("terrarium_tf_resources_by_mode_total", "mode" => "managed").set(managed as f64);
    gauge!("terrarium_tf_resources_by_mode_total", "mode" => "data").set(data as f64);
    gauge!("terrarium_tf_sensitive_outputs_total").set(sensitive_outputs as f64);

    let providers_dir = data_dir.join("registry").join("providers");
    gauge!("terrarium_registry_providers_total")
        .set(registry_provider_count(&providers_dir) as f64);
    gauge!("terrarium_registry_versions_total").set(registry_version_count(&providers_dir) as f64);
    gauge!("terrarium_registry_platform_archives_total")
        .set(registry_zip_count(&providers_dir) as f64);

    let mirror = app.mirror_status.read().await;
    gauge!("terrarium_registry_mirror_running").set(if mirror.running { 1.0 } else { 0.0 });
    if mirror.total_errors == 0 {
        if let Some(ts) = mirror.last_sync_finished {
            gauge!("terrarium_registry_mirror_last_success_timestamp_seconds").set(ts as f64);
        }
    }
    if let (Some(started), Some(finished)) = (mirror.last_sync_started, mirror.last_sync_finished) {
        if finished >= started {
            gauge!("terrarium_registry_mirror_last_duration_seconds").set((finished - started) as f64);
        }
    }
    drop(mirror);

    // ── Fleet composition (aggregate-only: keyed by version/provider/type,
    // never multiplied by workspace, so cardinality stays bounded) ──
    let fleet = fleet_composition(&app.state, &active_states);
    for (version, count) in &fleet.versions {
        gauge!("terrarium_tf_version_states", "version" => version.clone()).set(*count as f64);
    }
    for (provider, count) in &fleet.provider_resources {
        gauge!("terrarium_tf_provider_resources", "provider" => provider.clone()).set(*count as f64);
    }
    for (rtype, count) in &fleet.resource_types {
        gauge!("terrarium_tf_resource_type_total", "type" => rtype.clone()).set(*count as f64);
    }

    // ── Auth surface (aggregate-only, no usernames) ──
    let users = app.users.find_all().await;
    gauge!("terrarium_users_total").set(users.len() as f64);
    let (mut user_sessions, mut api_sessions) = (0usize, 0usize);
    for u in &users {
        // API keys are created with a name; interactive login sessions are not.
        // (SessionKind itself is not re-exported by authur, so we use that proxy.)
        for s in authur::Sessions::list_sessions(&app.users, u).await {
            if s.name.is_some() {
                api_sessions += 1;
            } else {
                user_sessions += 1;
            }
        }
    }
    gauge!("terrarium_auth_sessions_active", "kind" => "user").set(user_sessions as f64);
    gauge!("terrarium_auth_sessions_active", "kind" => "api").set(api_sessions as f64);

    if let Some(start) = START.get() {
        gauge!("terrarium_uptime_seconds").set(start.elapsed().as_secs_f64());
    }
}

/// Fleet-wide composition across all current states. Deliberately keyed only by
/// version / provider / resource-type — never by workspace — so the label
/// cardinality is bounded by how many distinct versions/providers/types exist,
/// not by workspace count.
#[derive(Default)]
struct Fleet {
    /// Number of states on each Terraform/OpenTofu version.
    versions: std::collections::HashMap<String, usize>,
    /// Resource count per provider across the fleet.
    provider_resources: std::collections::HashMap<String, usize>,
    /// Resource count per resource type across the fleet.
    resource_types: std::collections::HashMap<String, usize>,
}

fn fleet_composition(state: &crate::state::StateContainer, names: &[String]) -> Fleet {
    let mut fleet = Fleet::default();
    for name in names {
        let Some(bytes) = state.get(name) else { continue };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(v) = value.get("terraform_version").and_then(|v| v.as_str()) {
            let v = if v.is_empty() { "unknown" } else { v };
            *fleet.versions.entry(v.to_string()).or_default() += 1;
        }
        if let Some(arr) = value.get("resources").and_then(|v| v.as_array()) {
            for r in arr {
                if let Some(p) = normalize_provider(r.get("provider").and_then(|v| v.as_str())) {
                    *fleet.provider_resources.entry(p).or_default() += 1;
                }
                if let Some(t) = r.get("type").and_then(|v| v.as_str()) {
                    *fleet.resource_types.entry(t.to_string()).or_default() += 1;
                }
            }
        }
    }
    fleet
}

/// Reduce a Terraform provider reference to a stable short name.
/// e.g. `provider["registry.terraform.io/hashicorp/aws"]` -> `hashicorp/aws`.
fn normalize_provider(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let inner = raw
        .split_once('[')
        .and_then(|(_, rest)| rest.rsplit_once(']').map(|(v, _)| v))
        .unwrap_or(raw)
        .trim_matches('"');
    let short = inner.rsplit('/').take(2).collect::<Vec<_>>();
    if short.is_empty() {
        return None;
    }
    Some(short.into_iter().rev().collect::<Vec<_>>().join("/"))
}

fn tf_aggregates(
    state: &crate::state::StateContainer,
    names: &[String],
) -> (usize, usize, usize, usize, usize, usize) {
    let mut resources = 0;
    let mut managed = 0;
    let mut data = 0;
    let mut instances = 0;
    let mut outputs = 0;
    let mut sensitive_outputs = 0;

    for name in names {
        let Some(bytes) = state.get(name) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(map) = value.get("outputs").and_then(|v| v.as_object()) {
            outputs += map.len();
            sensitive_outputs += map
                .values()
                .filter(|v| {
                    v.get("sensitive")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                })
                .count();
        }
        if let Some(arr) = value.get("resources").and_then(|v| v.as_array()) {
            resources += arr.len();
            for r in arr {
                match r.get("mode").and_then(|v| v.as_str()).unwrap_or("managed") {
                    "data" => data += 1,
                    _ => managed += 1,
                }
                instances += r
                    .get("instances")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
            }
        }
    }

    (
        resources,
        managed,
        data,
        instances,
        outputs,
        sensitive_outputs,
    )
}

/// Terraform/OpenTofu state's own monotonically increasing write counter.
/// Useful for spotting a workspace that's drifted from what a client expects
/// (the classic "serial mismatch" concurrent-modification symptom).
pub fn state_serial(bytes: &[u8]) -> Option<u64> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    value.get("serial").and_then(|v| v.as_u64())
}

pub fn state_counts(bytes: &[u8]) -> Option<(usize, usize, usize)> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    let resources = value
        .get("resources")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let instances = value
        .get("resources")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    r.get("instances")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0);
    let outputs = value
        .get("outputs")
        .and_then(|v| v.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    Some((resources, instances, outputs))
}

pub fn observe_delta(metric: &'static str, workspace: &str, before: usize, after: usize) {
    let direction = if after > before {
        "positive"
    } else if after < before {
        "negative"
    } else {
        "zero"
    };
    let delta = after.abs_diff(before) as f64;
    histogram!(metric, "workspace" => workspace.to_string(), "direction" => direction).record(delta);
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn observe_lock_age(workspace: &str, info: &crate::lock::LockInfo) {
    if let Some(age) = lock_age_seconds(info) {
        histogram!("terrarium_lock_age_seconds", "workspace" => workspace.to_string()).record(age as f64);
    }
}

pub fn lock_operation(op: Option<&String>) -> &'static str {
    match op.map(|s| s.to_ascii_lowercase()) {
        Some(s) if s.contains("plan") => "plan",
        Some(s) if s.contains("apply") => "apply",
        Some(s) if s.contains("destroy") => "destroy",
        Some(s) if s.contains("refresh") => "refresh",
        Some(_) => "other",
        None => "unknown",
    }
}

fn max_lock_age(locks: &std::collections::HashMap<String, crate::lock::LockInfo>) -> u64 {
    locks
        .values()
        .filter_map(lock_age_seconds)
        .max()
        .unwrap_or(0)
}

fn lock_age_seconds(lock: &crate::lock::LockInfo) -> Option<u64> {
    let created = lock.Created.as_ref()?;
    let created = chrono::DateTime::parse_from_rfc3339(created).ok()?;
    let now = chrono::Utc::now();
    (now - created.with_timezone(&chrono::Utc))
        .to_std()
        .ok()
        .map(|d| d.as_secs())
}

/// Modification time of the newest retained version file for a workspace, as
/// a Unix timestamp. Used to backfill last-activity for workspaces that
/// haven't been pushed to since the server last started.
fn latest_version_mtime(state: &crate::state::StateContainer, name: &str) -> Option<u64> {
    let version = state.list_versions(name).last().copied()?;
    let path = state.versions_dir.join(name).join(version.to_string());
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn count_files(path: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() { count_files(&p) } else { 1 }
        })
        .sum()
}

fn registry_provider_count(path: &Path) -> usize {
    let Ok(ns) = std::fs::read_dir(path) else {
        return 0;
    };
    ns.filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|n| {
            std::fs::read_dir(n.path())
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

fn registry_version_count(path: &Path) -> usize {
    let Ok(ns) = std::fs::read_dir(path) else {
        return 0;
    };
    ns.filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|n| {
            std::fs::read_dir(n.path())
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|tp| {
                    std::fs::read_dir(tp.path())
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .filter(|e| e.path().is_dir())
                                .count()
                        })
                        .unwrap_or(0)
                })
                .sum::<usize>()
        })
        .sum()
}

fn registry_zip_count(path: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                registry_zip_count(&p)
            } else if p.extension().and_then(|e| e.to_str()) == Some("zip") {
                1
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, header};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_metrics_env() {
        unsafe {
            std::env::remove_var("TERRARIUM_METRICS_TOKEN");
            std::env::remove_var("TERRARIUM_METRICS_TOKEN_FILE");
        }
    }

    #[test]
    fn enabled_parses_truthy_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        unsafe { std::env::set_var("TERRARIUM_METRICS", "1") };
        assert!(enabled());
        unsafe { std::env::set_var("TERRARIUM_METRICS", "true") };
        assert!(enabled());
        unsafe { std::env::set_var("TERRARIUM_METRICS", "0") };
        assert!(!enabled());
        unsafe { std::env::remove_var("TERRARIUM_METRICS") };
    }

    #[test]
    fn metrics_token_prefers_token_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        let path =
            std::env::temp_dir().join(format!("terrarium-metrics-token-{}", std::process::id()));
        std::fs::write(&path, "from-file\n").unwrap();
        unsafe {
            std::env::set_var("TERRARIUM_METRICS_TOKEN", "from-env");
            std::env::set_var("TERRARIUM_METRICS_TOKEN_FILE", &path);
        }
        assert_eq!(metrics_token().as_deref(), Some("from-file"));
        let _ = std::fs::remove_file(path);
        clear_metrics_env();
    }

    #[tokio::test]
    async fn metrics_endpoint_rejects_missing_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        let response = metrics(HeaderMap::new()).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn metrics_endpoint_rejects_wrong_bearer_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        unsafe { std::env::set_var("TERRARIUM_METRICS_TOKEN", "secret") };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        let response = metrics(headers).await.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        clear_metrics_env();
    }

    #[test]
    fn state_counts_are_aggregate_only() {
        let state = br#"{
          "outputs": { "url": { "sensitive": false }, "password": { "sensitive": true } },
          "resources": [
            { "mode": "managed", "instances": [{}, {}] },
            { "mode": "data", "instances": [{}] }
          ]
        }"#;
        assert_eq!(state_counts(state), Some((2, 3, 2)));
    }

    #[test]
    fn state_serial_reads_the_write_counter() {
        assert_eq!(state_serial(br#"{"serial": 17}"#), Some(17));
        assert_eq!(state_serial(br#"{}"#), None);
        assert_eq!(state_serial(b"not json"), None);
    }

    #[test]
    fn normalize_provider_shortens_registry_refs() {
        assert_eq!(
            normalize_provider(Some(r#"provider["registry.terraform.io/hashicorp/aws"]"#)),
            Some("hashicorp/aws".to_string())
        );
        assert_eq!(
            normalize_provider(Some("provider.aws")),
            Some("provider.aws".to_string())
        );
        assert_eq!(normalize_provider(None), None);
    }

    #[test]
    fn scrub_path_normalizes_sensitive_paths() {
        assert_eq!(scrub_path("/state/infra/prod"), "/state/{*name}");
        assert_eq!(scrub_path("/lock/infra/prod"), "/lock/{*name}");
        assert_eq!(
            scrub_path("/registry/providers/ns/type/1/linux/amd64/zip"),
            "/registry/..."
        );
    }

    #[test]
    fn observe_delta_includes_workspace_label() {
        let _guard = ENV_LOCK.lock().unwrap();
        let recorder = metrics_util::debugging::DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        // Only one test in the whole run may install a global recorder; keep
        // this the sole test that does so.
        if recorder.install().is_err() {
            return;
        }

        observe_delta("terrarium_tf_resource_delta", "infra/prod", 1, 3);

        let has_workspace_label = snapshotter.snapshot().into_vec().iter().any(|(key, ..)| {
            key.key().name() == "terrarium_tf_resource_delta"
                && key
                    .key()
                    .labels()
                    .any(|l| l.key() == "workspace" && l.value() == "infra/prod")
        });
        assert!(has_workspace_label, "expected workspace label on delta histogram");
    }
}
