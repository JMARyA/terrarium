//! Terraform/OpenTofu provider registry.
//!
//! Implements three surface areas:
//!
//!  1. **Provider Registry Protocol v1** — the standard API OpenTofu queries
//!     when `source = "hostname/namespace/type"` points at this server and no
//!     mirror is configured.
//!
//!  2. **Upload + download** — authenticated push of provider zip archives and
//!     optional Markdown documentation.
//!
//!  3. **Network Mirror Protocol** — the lighter-weight mirror API that OpenTofu
//!     uses when a `network_mirror { url = "…/registry/mirror/" }` block is
//!     present in the CLI config.  No GPG signatures required; providers are
//!     verified by `zh:` (zip-SHA256) hashes.
//!
//!  4. **Upstream mirroring** — providers are fetched-and-cached either by a
//!     manual POST or lazily when the network mirror is queried.

use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::auth::AuthUser;

// ── Mirror status ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderSyncStatus {
    pub ok: usize,
    pub errors: usize,
    pub last_errors: Vec<String>,
    pub last_synced: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MirrorStatus {
    pub last_sync_started: Option<u64>,
    pub last_sync_finished: Option<u64>,
    pub running: bool,
    pub total_ok: usize,
    pub total_errors: usize,
    pub providers: std::collections::HashMap<String, ProviderSyncStatus>,
}

pub type MirrorStatusRef = Arc<RwLock<MirrorStatus>>;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Storage ────────────────────────────────────────────────────────────────

/// Thin wrapper around the on-disk provider store.
///
/// Layout:
/// ```text
/// registry/providers/{namespace}/{type}/{version}/
///   meta.json           ← VersionMeta (protocols, signing_keys, docs)
///   {os}_{arch}/
///     terraform-provider-{type}_{version}_{os}_{arch}.zip
///     sha256              ← hex SHA-256 of the zip
/// ```
#[derive(Clone)]
pub struct RegistryStore {
    dir: PathBuf,
}

impl RegistryStore {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn providers_dir(&self) -> PathBuf { self.dir.join("providers") }
    fn provider_dir(&self, ns: &str, tp: &str) -> PathBuf { self.providers_dir().join(ns).join(tp) }
    fn version_dir(&self, ns: &str, tp: &str, ver: &str) -> PathBuf { self.provider_dir(ns, tp).join(ver) }
    fn platform_dir(&self, ns: &str, tp: &str, ver: &str, os: &str, arch: &str) -> PathBuf {
        self.version_dir(ns, tp, ver).join(format!("{os}_{arch}"))
    }

    fn docs_dir(&self, ns: &str, tp: &str, ver: &str) -> PathBuf {
        self.version_dir(ns, tp, ver).join("docs")
    }

    pub fn has_docs(&self, ns: &str, tp: &str, ver: &str) -> bool {
        self.docs_dir(ns, tp, ver).is_dir()
    }

    pub fn get_doc_file(&self, ns: &str, tp: &str, ver: &str, rel: &str) -> Option<String> {
        let base = self.docs_dir(ns, tp, ver).join(rel);
        for ext in &[".md", ".mdx"] {
            let mut p = base.clone();
            let mut name = p.file_name()?.to_string_lossy().into_owned();
            name.push_str(ext);
            p.set_file_name(name);
            if let Ok(s) = std::fs::read_to_string(&p) { return Some(s); }
        }
        if let Ok(s) = std::fs::read_to_string(&base) { return Some(s); }
        None
    }

    pub fn list_doc_category(&self, ns: &str, tp: &str, ver: &str, category: &str) -> Vec<String> {
        let dir = self.docs_dir(ns, tp, ver).join(category);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .into_iter().flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.ends_with(".md")  { Some(n[..n.len()-3].to_string()) }
                else if n.ends_with(".mdx") { Some(n[..n.len()-4].to_string()) }
                else { None }
            })
            .collect();
        names.sort();
        names
    }

    pub fn store_doc_file(&self, ns: &str, tp: &str, ver: &str, rel: &str, data: &[u8]) {
        let dest = self.docs_dir(ns, tp, ver).join(rel);
        if let Some(p) = dest.parent() { std::fs::create_dir_all(p).ok(); }
        std::fs::write(dest, data).ok();
    }

    pub fn store_docs_bundle(&self, ns: &str, tp: &str, ver: &str, zip_bytes: &[u8]) -> Result<usize, String> {
        let cursor = std::io::Cursor::new(zip_bytes);
        let mut archive = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;
        let mut count = 0;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
            if file.is_dir() { continue; }
            let raw_name = file.name().to_string();
            // Normalize: strip leading "docs/" prefix if present
            let rel = raw_name.strip_prefix("docs/").unwrap_or(&raw_name).to_string();
            let valid = matches!(rel.as_str(), "index.md" | "index.mdx")
                || rel.starts_with("resources/")
                || rel.starts_with("data-sources/")
                || rel.starts_with("guides/")
                || rel.starts_with("functions/")
                || rel.starts_with("ephemeral-resources/");
            if !valid { continue; }
            let mut content = Vec::new();
            file.read_to_end(&mut content).map_err(|e| e.to_string())?;
            self.store_doc_file(ns, tp, ver, &rel, &content);
            count += 1;
        }
        Ok(count)
    }

    pub fn list_providers(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for ns_entry in read_subdirs(&self.providers_dir()) {
            let ns = entry_name(&ns_entry);
            for tp_entry in read_subdirs(&ns_entry.path()) {
                out.push((ns.clone(), entry_name(&tp_entry)));
            }
        }
        out.sort();
        out
    }

    pub fn list_versions(&self, ns: &str, tp: &str) -> Vec<String> {
        let mut vs: Vec<String> = read_subdirs(&self.provider_dir(ns, tp))
            .map(|e| entry_name(&e))
            .collect();
        vs.sort_by(|a, b| semver_cmp(a, b));
        vs
    }

    pub fn list_platforms(&self, ns: &str, tp: &str, ver: &str) -> Vec<Platform> {
        read_subdirs(&self.version_dir(ns, tp, ver))
            .filter_map(|e| {
                let name = entry_name(&e);
                let (os, arch) = name.split_once('_')?;
                Some(Platform { os: os.into(), arch: arch.into() })
            })
            .collect()
    }

    pub fn get_meta(&self, ns: &str, tp: &str, ver: &str) -> Option<VersionMeta> {
        let raw = std::fs::read_to_string(self.version_dir(ns, tp, ver).join("meta.json")).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save_meta(&self, ns: &str, tp: &str, ver: &str, meta: &VersionMeta) {
        let dir = self.version_dir(ns, tp, ver);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(meta).unwrap()).unwrap();
    }

    pub fn get_zip(&self, ns: &str, tp: &str, ver: &str, os: &str, arch: &str) -> Option<Vec<u8>> {
        let fname = zip_filename(tp, ver, os, arch);
        std::fs::read(self.platform_dir(ns, tp, ver, os, arch).join(fname)).ok()
    }

    pub fn get_sha256(&self, ns: &str, tp: &str, ver: &str, os: &str, arch: &str) -> Option<String> {
        let raw = std::fs::read_to_string(self.platform_dir(ns, tp, ver, os, arch).join("sha256")).ok()?;
        Some(raw.trim().to_string())
    }

    pub fn store_binary(
        &self,
        ns: &str, tp: &str, ver: &str, os: &str, arch: &str,
        data: &[u8],
        protocols: Vec<String>,
        signing_keys: SigningKeys,
        docs: Option<String>,
    ) {
        let dir = self.platform_dir(ns, tp, ver, os, arch);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(zip_filename(tp, ver, os, arch)), data).unwrap();
        std::fs::write(dir.join("sha256"), hex_sha256(data)).unwrap();

        let mut meta = self.get_meta(ns, tp, ver).unwrap_or_default();
        if !protocols.is_empty()                    { meta.protocols = protocols; }
        if !signing_keys.gpg_public_keys.is_empty() { meta.signing_keys = signing_keys; }
        if docs.is_some()                           { meta.docs = docs; }
        self.save_meta(ns, tp, ver, &meta);
    }
}

// ── Data types ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Platform { pub os: String, pub arch: String }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SigningKeys { pub gpg_public_keys: Vec<GpgKey> }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GpgKey {
    pub key_id: String,
    pub ascii_armor: String,
    #[serde(default)] pub trust_signature: String,
    #[serde(default)] pub source: String,
    #[serde(default)] pub source_url: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct VersionMeta {
    pub protocols: Vec<String>,
    pub signing_keys: SigningKeys,
    pub docs: Option<String>,
}

impl Default for VersionMeta {
    fn default() -> Self {
        Self { protocols: vec!["5.0".into(), "6.0".into()], signing_keys: Default::default(), docs: None }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn zip_filename(tp: &str, ver: &str, os: &str, arch: &str) -> String {
    format!("terraform-provider-{tp}_{ver}_{os}_{arch}.zip")
}

fn hex_sha256(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

/// zh: hash — SHA-256 of the zip bytes, base64-encoded.
/// OpenTofu accepts this in network mirror version JSON.
fn zh_hash(data: &[u8]) -> String {
    format!("zh:{}", BASE64.encode(Sha256::digest(data)))
}

fn read_subdirs(dir: &std::path::Path) -> impl Iterator<Item = std::fs::DirEntry> {
    std::fs::read_dir(dir).into_iter().flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
}

fn entry_name(e: &std::fs::DirEntry) -> String {
    e.file_name().to_string_lossy().into_owned()
}

fn semver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let p = |s: &str| -> Vec<u64> { s.split('.').filter_map(|x| x.parse().ok()).collect() };
    p(a).cmp(&p(b))
}

fn upstream_registry_allowed(host: &str) -> bool {
    if host.is_empty() || host.contains('/') || host.contains(':') || host.contains('@') {
        return false;
    }

    let allowed = std::env::var("TERRARIUM_UPSTREAM_REGISTRIES")
        .unwrap_or_else(|_| "registry.terraform.io,registry.opentofu.org".into());

    allowed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|allowed_host| allowed_host == host)
}

fn upstream_api_url(host: &str, path: &str) -> Option<String> {
    upstream_registry_allowed(host).then(|| format!("https://{host}{path}"))
}

async fn fetch_upstream_versions(client: &reqwest::Client, host: &str, ns: &str, tp: &str) -> Result<Value, String> {
    let url = upstream_api_url(host, &format!("/v1/providers/{ns}/{tp}/versions"))
        .ok_or_else(|| format!("upstream registry not allowed: {host}"))?;
    let resp = client
        .get(url)
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    resp.json().await.map_err(|e| e.to_string())
}

fn versions_from_upstream_json(upstream: &Value) -> Vec<String> {
    let mut versions: Vec<String> = upstream["versions"].as_array().unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["version"].as_str().map(String::from))
        .collect();
    versions.sort_by(|a, b| semver_cmp(a, b));
    versions
}

fn platforms_for_version(upstream: &Value, ver: &str) -> Vec<Platform> {
    upstream["versions"].as_array().unwrap_or(&vec![])
        .iter()
        .find(|v| v["version"].as_str() == Some(ver))
        .and_then(|v| v["platforms"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| Some(Platform {
            os: p["os"].as_str()?.into(),
            arch: p["arch"].as_str()?.into(),
        }))
        .collect()
}

async fn mirror_upstream_platform(
    app: &AppState,
    client: &reqwest::Client,
    host: &str,
    ns: &str,
    tp: &str,
    ver: &str,
    p: &Platform,
) -> Result<bool, String> {
    if app.registry.get_sha256(ns, tp, ver, &p.os, &p.arch).is_some() {
        return Ok(false);
    }

    let info_url = upstream_api_url(host, &format!(
        "/v1/providers/{ns}/{tp}/{ver}/download/{}/{}",
        p.os, p.arch
    )).ok_or_else(|| format!("upstream registry not allowed: {host}"))?;

    let info: Value = client
        .get(info_url)
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json().await
        .map_err(|e| e.to_string())?;

    let durl = info["download_url"].as_str().ok_or_else(|| "missing download_url".to_string())?;
    let zip = client
        .get(durl)
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes().await
        .map_err(|e| e.to_string())?;

    let signing_keys: SigningKeys = serde_json::from_value(info["signing_keys"].clone()).unwrap_or_default();
    let protocols: Vec<String> = info["protocols"].as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    app.registry.store_binary(ns, tp, ver, &p.os, &p.arch, &zip, protocols, signing_keys, None);
    tracing::info!("🪞 lazily mirrored {host}/{ns}/{tp} {ver} {}_{}", p.os, p.arch);
    Ok(true)
}

async fn ensure_upstream_version_cached(app: &AppState, host: &str, ns: &str, tp: &str, ver: &str) {
    let client = reqwest::Client::new();
    let upstream = match fetch_upstream_versions(&client, host, ns, tp).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("lazy mirror: cannot list {host}/{ns}/{tp}: {e}");
            return;
        }
    };

    for p in platforms_for_version(&upstream, ver) {
        if let Err(e) = mirror_upstream_platform(app, &client, host, ns, tp, ver, &p).await {
            tracing::debug!("lazy mirror: {host}/{ns}/{tp} {ver} {}_{}: {e}", p.os, p.arch);
        }
    }
}

// ── Service discovery ──────────────────────────────────────────────────────

pub async fn service_discovery() -> Json<Value> {
    Json(json!({ "providers.v1": "/registry/v1/providers/" }))
}

// ── Provider Registry Protocol v1 ──────────────────────────────────────────

/// `GET /registry/v1/providers/{namespace}/{type}/versions`
pub async fn list_versions(
    State(app): State<AppState>,
    Path((ns, tp)): Path<(String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let versions = app.registry.list_versions(&ns, &tp);
    if versions.is_empty() { return Err(StatusCode::NOT_FOUND); }

    let versions_json: Vec<Value> = versions.iter().map(|ver| {
        let meta = app.registry.get_meta(&ns, &tp, ver).unwrap_or_default();
        let platforms = app.registry.list_platforms(&ns, &tp, ver);
        json!({
            "version": ver,
            "protocols": meta.protocols,
            "platforms": platforms.iter().map(|p| json!({"os": p.os, "arch": p.arch})).collect::<Vec<_>>(),
        })
    }).collect();

    Ok(Json(json!({ "versions": versions_json })))
}

/// `GET /registry/v1/providers/{namespace}/{type}/{version}/download/{os}/{arch}`
pub async fn download_info(
    State(app): State<AppState>,
    Path((ns, tp, ver, os, arch)): Path<(String, String, String, String, String)>,
) -> Result<Json<Value>, StatusCode> {
    let meta    = app.registry.get_meta(&ns, &tp, &ver).ok_or(StatusCode::NOT_FOUND)?;
    let sha256  = app.registry.get_sha256(&ns, &tp, &ver, &os, &arch).ok_or(StatusCode::NOT_FOUND)?;
    // Verify the zip actually exists
    app.registry.get_zip(&ns, &tp, &ver, &os, &arch).ok_or(StatusCode::NOT_FOUND)?;
    let filename = zip_filename(&tp, &ver, &os, &arch);

    Ok(Json(json!({
        "protocols":              meta.protocols,
        "os":                     os,
        "arch":                   arch,
        "filename":               filename,
        "download_url":           format!("/registry/providers/{ns}/{tp}/{ver}/{os}/{arch}/zip"),
        "shasums_url":            format!("/registry/providers/{ns}/{tp}/{ver}/SHA256SUMS"),
        "shasums_signature_url":  format!("/registry/providers/{ns}/{tp}/{ver}/SHA256SUMS.sig"),
        "shasum":                 sha256,
        "signing_keys":           meta.signing_keys,
    })))
}

// ── Upload ─────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct UploadQuery {
    /// Comma-separated protocol versions, e.g. `5.0,6.0` (default: `5.0,6.0`).
    pub protocols: Option<String>,
    /// Inline Markdown documentation for the provider.
    pub docs: Option<String>,
}

/// `POST /registry/providers/{namespace}/{type}/{version}/{os}/{arch}`
pub async fn upload_provider(
    _auth: AuthUser,
    State(app): State<AppState>,
    Path((ns, tp, ver, os, arch)): Path<(String, String, String, String, String)>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> StatusCode {
    if body.is_empty() {
        metrics::counter!("terrarium_registry_uploads_total", "result" => "bad_request").increment(1);
        return StatusCode::BAD_REQUEST;
    }

    let protocols = q.protocols.as_deref()
        .map(|p| p.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_else(|| vec!["5.0".into(), "6.0".into()]);

    app.registry.store_binary(&ns, &tp, &ver, &os, &arch, &body, protocols, SigningKeys::default(), q.docs);
    metrics::counter!("terrarium_registry_uploads_total", "result" => "ok").increment(1);
    tracing::info!("📦 Registry: uploaded {ns}/{tp} {ver} {os}_{arch}");
    StatusCode::OK
}

/// `PUT /registry/providers/{namespace}/{type}/{version}/docs`
///
/// Upload structured documentation for a provider version.
///
/// Body can be either:
/// - A ZIP archive containing `docs/` in the `terraform-plugin-docs` layout:
///   `docs/index.md`, `docs/resources/*.md`, `docs/data-sources/*.md`, etc.
/// - Plain Markdown — stored as the provider overview (`index.md`).
pub async fn upload_docs(
    _auth: AuthUser,
    State(app): State<AppState>,
    Path((ns, tp, ver)): Path<(String, String, String)>,
    body: Bytes,
) -> impl IntoResponse {
    if body.is_empty() { return (StatusCode::BAD_REQUEST, "empty body").into_response(); }

    if body.starts_with(b"PK\x03\x04") {
        match app.registry.store_docs_bundle(&ns, &tp, &ver, &body) {
            Ok(n) => {
                tracing::info!("📄 Registry: docs bundle {ns}/{tp} {ver} ({n} files)");
                StatusCode::OK.into_response()
            }
            Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        }
    } else {
        app.registry.store_doc_file(&ns, &tp, &ver, "index.md", &body);
        tracing::info!("📄 Registry: docs index {ns}/{tp} {ver}");
        StatusCode::OK.into_response()
    }
}

// ── Serve binary ───────────────────────────────────────────────────────────

/// `GET /registry/providers/{namespace}/{type}/{version}/{os}/{arch}/zip`
pub async fn serve_binary(
    State(app): State<AppState>,
    Path((ns, tp, ver, os, arch)): Path<(String, String, String, String, String)>,
) -> Result<Response, StatusCode> {
    let data = match app.registry.get_zip(&ns, &tp, &ver, &os, &arch) {
        Some(data) => data,
        None => {
            metrics::counter!("terrarium_registry_downloads_total", "result" => "not_found").increment(1);
            return Err(StatusCode::NOT_FOUND);
        }
    };
    metrics::counter!("terrarium_registry_downloads_total", "result" => "ok").increment(1);
    let fname = zip_filename(&tp, &ver, &os, &arch);
    Ok((
        [(header::CONTENT_TYPE, "application/zip"),
         (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{fname}\"").as_str())],
        data,
    ).into_response())
}

// ── Upstream mirror ─────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone)]
pub struct MirrorRequest {
    pub namespace: String,
    #[serde(rename = "type")]
    pub type_: String,
    /// Versions to mirror. If absent, all available versions are mirrored.
    pub versions: Option<Vec<String>>,
    /// Platforms to mirror. Defaults to the five most common ones.
    pub platforms: Option<Vec<Platform>>,
}

/// Core mirror logic — shared by the HTTP handler and the auto-mirror startup task.
pub async fn perform_mirror(app: &AppState, req: MirrorRequest) -> (Vec<String>, Vec<String>) {
    let client = reqwest::Client::new();
    let ns = &req.namespace;
    let tp = &req.type_;

    let upstream: Value = match client
        .get(format!("https://registry.terraform.io/v1/providers/{ns}/{tp}/versions"))
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => match r.json().await { Ok(v) => v, Err(e) => return (vec![], vec![e.to_string()]) },
        Err(e) => return (vec![], vec![e.to_string()]),
    };

    let all_versions: Vec<String> = upstream["versions"].as_array().unwrap_or(&vec![])
        .iter().filter_map(|v| v["version"].as_str().map(String::from)).collect();

    let want_versions: Vec<String> = match &req.versions {
        Some(r) => all_versions.into_iter().filter(|v| r.contains(v)).collect(),
        None    => all_versions,
    };

    let platforms = req.platforms.clone().unwrap_or_else(|| vec![
        Platform { os: "linux".into(),   arch: "amd64".into() },
        Platform { os: "linux".into(),   arch: "arm64".into() },
        Platform { os: "darwin".into(),  arch: "amd64".into() },
        Platform { os: "darwin".into(),  arch: "arm64".into() },
        Platform { os: "windows".into(), arch: "amd64".into() },
    ]);

    let mut mirrored = vec![];
    let mut errors   = vec![];

    for ver in &want_versions {
        for p in &platforms {
            // Skip artifacts already in the store (idempotent re-runs)
            if app.registry.get_sha256(ns, tp, ver, &p.os, &p.arch).is_some() {
                tracing::debug!("⏭ {ns}/{tp} {ver} {}_{} already stored", p.os, p.arch);
                continue;
            }

            let dl_url = format!(
                "https://registry.terraform.io/v1/providers/{ns}/{tp}/{ver}/download/{}/{}",
                p.os, p.arch
            );
            let info = match client.get(&dl_url)
                .header("User-Agent", "terrarium-mirror/1.0")
                .send().await
            {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => None,
            };
            let Some(info) = info else {
                tracing::debug!("⏭ {ns}/{tp} {ver} {}_{}: not available upstream", p.os, p.arch);
                continue;
            };
            let Some(durl) = info["download_url"].as_str() else { continue; };

            match client.get(durl).header("User-Agent", "terrarium-mirror/1.0").send().await {
                Ok(r) if r.status().is_success() => {
                    match r.bytes().await {
                        Ok(zip) => {
                            let signing_keys: SigningKeys = serde_json::from_value(
                                info["signing_keys"].clone()
                            ).unwrap_or_default();
                            let protocols: Vec<String> = info["protocols"].as_array()
                                .unwrap_or(&vec![])
                                .iter().filter_map(|v| v.as_str().map(String::from)).collect();
                            app.registry.store_binary(ns, tp, ver, &p.os, &p.arch, &zip, protocols, signing_keys, None);
                            mirrored.push(format!("{ver} {}_{}", p.os, p.arch));
                        }
                        Err(e) => errors.push(format!("{ver} {}_{}: {e}", p.os, p.arch)),
                    }
                }
                Ok(r)  => errors.push(format!("{ver} {}_{}: HTTP {}", p.os, p.arch, r.status())),
                Err(e) => errors.push(format!("{ver} {}_{}: {e}", p.os, p.arch)),
            }
        }
    }

    // Fetch docs from GitHub for each successfully mirrored version.
    if let Some(source_url) = fetch_provider_source(&client, ns, tp).await {
        if let Some((owner, repo)) = parse_github_url(&source_url) {
            for ver in &want_versions {
                mirror_docs_from_github(&client, &app.registry, ns, tp, ver, &owner, &repo).await;
            }
        }
    }

    (mirrored, errors)
}

/// `GET /registry/status`
pub async fn registry_status(State(app): State<AppState>) -> Json<Value> {
    let s = app.mirror_status.read().await;
    Json(serde_json::to_value(&*s).unwrap_or(json!({})))
}

/// `POST /registry/mirror`
///
/// Fetches providers from `registry.terraform.io` and stores them locally.
pub async fn mirror_upstream(
    _auth: AuthUser,
    State(app): State<AppState>,
    Json(req): Json<MirrorRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (mirrored, errors) = perform_mirror(&app, req).await;
    metrics::counter!("terrarium_registry_mirror_runs_total", "result" => if errors.is_empty() { "ok" } else { "error" }).increment(1);
    Ok(Json(json!({ "mirrored": mirrored, "errors": errors })))
}

/// Run all mirrors listed in `mirrors.json`. Returns the total error count.
/// Called on startup (with retries) and on each periodic interval tick.
pub async fn run_auto_mirrors(app: AppState, mirrors_path: std::path::PathBuf) -> usize {
    let content = match std::fs::read_to_string(&mirrors_path) {
        Ok(s) => s,
        Err(e) => { tracing::warn!("Cannot read mirrors.json: {e}"); return 0; }
    };
    let raw: Vec<MirrorRequest> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => { tracing::warn!("Invalid mirrors.json: {e}"); return 0; }
    };

    // Validate and deduplicate
    let mut seen = std::collections::HashSet::new();
    let mirrors: Vec<MirrorRequest> = raw.into_iter().filter(|req| {
        if req.namespace.is_empty() || req.type_.is_empty() {
            tracing::warn!("mirrors.json: skipping entry with empty namespace or type");
            return false;
        }
        let key = format!("{}/{}", req.namespace, req.type_);
        if !seen.insert(key.clone()) {
            tracing::warn!("mirrors.json: duplicate entry {key}, skipping");
            return false;
        }
        true
    }).collect();

    let started = unix_now();
    {
        let mut s = app.mirror_status.write().await;
        s.running = true;
        s.last_sync_started = Some(started);
    }

    tracing::info!("🔄 Auto-mirroring {} provider(s)", mirrors.len());
    let mut total_ok = 0usize;
    let mut total_err = 0usize;

    for req in mirrors {
        let (ns, tp) = (req.namespace.clone(), req.type_.clone());
        let (mirrored, errors) = perform_mirror(&app, req).await;
        let ok = mirrored.len();
        let err = errors.len();
        total_ok += ok;
        total_err += err;

        if errors.is_empty() {
            tracing::info!("🪞 {ns}/{tp}: {ok} mirrored");
        } else {
            tracing::warn!("🪞 {ns}/{tp}: {ok} ok, {err} error(s): {}", errors.join("; "));
        }

        let now = unix_now();
        let mut s = app.mirror_status.write().await;
        let pstatus = s.providers.entry(format!("{ns}/{tp}")).or_default();
        pstatus.ok += ok;
        pstatus.errors += err;
        if !errors.is_empty() {
            pstatus.last_errors = errors;
        }
        pstatus.last_synced = Some(now);
    }

    let finished = unix_now();
    {
        let mut s = app.mirror_status.write().await;
        s.running = false;
        s.last_sync_finished = Some(finished);
        s.total_ok += total_ok;
        s.total_errors += total_err;
    }

    tracing::info!("✅ mirror sync done: {total_ok} artifact(s), {total_err} error(s)");
    metrics::counter!("terrarium_registry_mirror_runs_total", "result" => if total_err == 0 { "ok" } else { "error" }).increment(1);
    total_err
}

async fn fetch_provider_source(client: &reqwest::Client, ns: &str, tp: &str) -> Option<String> {
    let info: Value = client
        .get(format!("https://registry.terraform.io/v1/providers/{ns}/{tp}"))
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await.ok()?
        .json().await.ok()?;
    info["source"].as_str().map(String::from)
}

fn parse_github_url(url: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = url.trim_end_matches('/').rsplitn(3, '/').collect();
    // rsplitn gives [repo, owner, prefix] in reverse
    if parts.len() >= 2 {
        Some((parts[1].to_string(), parts[0].to_string()))
    } else {
        None
    }
}

async fn mirror_docs_from_github(
    client: &reqwest::Client,
    registry: &RegistryStore,
    ns: &str,
    tp: &str,
    ver: &str,
    owner: &str,
    repo: &str,
) {
    let tag = format!("v{ver}");
    let tree_url = format!(
        "https://api.github.com/repos/{owner}/{repo}/git/trees/{tag}?recursive=1"
    );
    let tree: Value = match client
        .get(&tree_url)
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await
    {
        Ok(r) if r.status().is_success() => match r.json().await { Ok(v) => v, Err(_) => return },
        _ => return,
    };

    let doc_paths: Vec<String> = tree["tree"].as_array().unwrap_or(&vec![])
        .iter()
        .filter_map(|e| {
            let path = e["path"].as_str()?;
            let is_blob = e["type"].as_str() == Some("blob");
            // Only the non-cdktf docs/ tree, .md or .mdx files
            let is_doc = path.starts_with("docs/")
                && !path.starts_with("docs/cdktf/")
                && (path.ends_with(".md") || path.ends_with(".mdx"));
            if is_blob && is_doc { Some(path.to_string()) } else { None }
        })
        .collect();

    let mut count = 0;
    for path in doc_paths {
        let raw_url = format!(
            "https://raw.githubusercontent.com/{owner}/{repo}/{tag}/{path}"
        );
        if let Ok(resp) = client.get(&raw_url)
            .header("User-Agent", "terrarium-mirror/1.0")
            .send().await
        {
            if resp.status().is_success() {
                if let Ok(bytes) = resp.bytes().await {
                    let rel = path.strip_prefix("docs/").unwrap_or(&path);
                    registry.store_doc_file(ns, tp, ver, rel, &bytes);
                    count += 1;
                }
            }
        }
    }
    tracing::info!("📚 Docs {ns}/{tp} {ver}: {count} files from {owner}/{repo}@{tag}");
}

// ── Network Mirror Protocol ─────────────────────────────────────────────────
//
// When OpenTofu is configured with:
//   provider_installation {
//     network_mirror { url = "http://host/registry/mirror/" }
//   }
//
// it requests:
//   GET /registry/mirror/{source_host}/{namespace}/{type}/index.json
//   GET /registry/mirror/{source_host}/{namespace}/{type}/{version}.json
//   GET /registry/mirror/{source_host}/{namespace}/{type}/{filename}.zip
//
// The `source_host` prefix is stripped (it's the registry host from the
// provider `source` attribute) leaving just namespace/type as the storage key.

/// `GET /registry/mirror/{*path}`
pub async fn network_mirror(
    State(app): State<AppState>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    // path = "{source_host}/{namespace}/{type}/{filename}"
    // splitn(4) gives exactly [host, namespace, type, filename].
    let parts: Vec<&str> = path.splitn(4, '/').collect();
    if parts.len() < 4 { return StatusCode::NOT_FOUND.into_response(); }
    let (host, ns, tp, rest) = (parts[0], parts[1], parts[2], parts[3]);

    // ── index.json ──
    if rest == "index.json" {
        let mut versions = app.registry.list_versions(ns, tp);

        // On-demand mode: merge the local cache with the upstream registry that
        // appears in the mirror path (for example registry.opentofu.org).  This
        // lets OpenTofu discover newly published versions without predeclaring
        // them in mirrors.json.
        if upstream_registry_allowed(host) {
            let client = reqwest::Client::new();
            match fetch_upstream_versions(&client, host, ns, tp).await {
                Ok(upstream) => {
                    versions.extend(versions_from_upstream_json(&upstream));
                    versions.sort_by(|a, b| semver_cmp(a, b));
                    versions.dedup();
                }
                Err(e) => tracing::debug!("lazy mirror index: cannot list {host}/{ns}/{tp}: {e}"),
            }
        }

        if versions.is_empty() { return StatusCode::NOT_FOUND.into_response(); }
        let map: serde_json::Map<String, Value> =
            versions.into_iter().map(|v| (v, json!({}))).collect();
        return Json(json!({ "versions": Value::Object(map) })).into_response();
    }

    // ── {version}.json ──
    if let Some(ver) = rest.strip_suffix(".json") {
        if upstream_registry_allowed(host) {
            ensure_upstream_version_cached(&app, host, ns, tp, ver).await;
        }

        let platforms = app.registry.list_platforms(ns, tp, ver);
        if platforms.is_empty() { return StatusCode::NOT_FOUND.into_response(); }

        let mut archives = serde_json::Map::new();
        for p in &platforms {
            let key = format!("{}_{}", p.os, p.arch);
            // Relative URL — resolved against this JSON's directory by tofu.
            let url = zip_filename(tp, ver, &p.os, &p.arch);
            let hashes = app.registry.get_zip(ns, tp, ver, &p.os, &p.arch)
                .map(|z| vec![zh_hash(&z)])
                .unwrap_or_default();
            archives.insert(key, json!({ "url": url, "hashes": hashes }));
        }
        return Json(json!({ "archives": Value::Object(archives) })).into_response();
    }

    // ── {filename}.zip ──
    if rest.ends_with(".zip") {
        // Parse version, os, arch from the filename convention:
        // terraform-provider-{type}_{version}_{os}_{arch}.zip
        let prefix = format!("terraform-provider-{tp}_");
        if let Some(suffix) = rest.strip_prefix(&prefix).and_then(|s| s.strip_suffix(".zip")) {
            let mut versions = app.registry.list_versions(ns, tp);
            if upstream_registry_allowed(host) {
                let client = reqwest::Client::new();
                if let Ok(upstream) = fetch_upstream_versions(&client, host, ns, tp).await {
                    versions.extend(versions_from_upstream_json(&upstream));
                    versions.sort_by(|a, b| semver_cmp(a, b));
                    versions.dedup();
                }
            }

            for ver in versions {
                if let Some(os_arch) = suffix.strip_prefix(&format!("{ver}_")) {
                    if let Some((os, arch)) = os_arch.split_once('_') {
                        if let Some(data) = app.registry.get_zip(ns, tp, &ver, os, arch) {
                            return ([(header::CONTENT_TYPE, "application/zip")], data).into_response();
                        }

                        if upstream_registry_allowed(host) {
                            let client = reqwest::Client::new();
                            let p = Platform { os: os.into(), arch: arch.into() };
                            if mirror_upstream_platform(&app, &client, host, ns, tp, &ver, &p).await.is_ok() {
                                if let Some(data) = app.registry.get_zip(ns, tp, &ver, os, arch) {
                                    return ([(header::CONTENT_TYPE, "application/zip")], data).into_response();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    StatusCode::NOT_FOUND.into_response()
}
