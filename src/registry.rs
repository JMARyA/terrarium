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
//!  4. **Upstream mirroring** — one POST triggers a fetch-and-cache of a
//!     provider (or set of versions/platforms) from registry.terraform.io.

use std::path::PathBuf;

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
    if body.is_empty() { return StatusCode::BAD_REQUEST; }

    let protocols = q.protocols.as_deref()
        .map(|p| p.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_else(|| vec!["5.0".into(), "6.0".into()]);

    app.registry.store_binary(&ns, &tp, &ver, &os, &arch, &body, protocols, SigningKeys::default(), q.docs);
    tracing::info!("📦 Registry: uploaded {ns}/{tp} {ver} {os}_{arch}");
    StatusCode::OK
}

/// `PUT /registry/providers/{namespace}/{type}/{version}/docs`
///
/// Replace the Markdown documentation for a provider version.
pub async fn upload_docs(
    _auth: AuthUser,
    State(app): State<AppState>,
    Path((ns, tp, ver)): Path<(String, String, String)>,
    body: Bytes,
) -> StatusCode {
    let Some(mut meta) = app.registry.get_meta(&ns, &tp, &ver) else {
        return StatusCode::NOT_FOUND;
    };
    meta.docs = Some(String::from_utf8_lossy(&body).into_owned());
    app.registry.save_meta(&ns, &tp, &ver, &meta);
    StatusCode::OK
}

// ── Serve binary ───────────────────────────────────────────────────────────

/// `GET /registry/providers/{namespace}/{type}/{version}/{os}/{arch}/zip`
pub async fn serve_binary(
    State(app): State<AppState>,
    Path((ns, tp, ver, os, arch)): Path<(String, String, String, String, String)>,
) -> Result<Response, StatusCode> {
    let data = app.registry.get_zip(&ns, &tp, &ver, &os, &arch).ok_or(StatusCode::NOT_FOUND)?;
    let fname = zip_filename(&tp, &ver, &os, &arch);
    Ok((
        [(header::CONTENT_TYPE, "application/zip"),
         (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{fname}\"").as_str())],
        data,
    ).into_response())
}

// ── Upstream mirror ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MirrorRequest {
    pub namespace: String,
    #[serde(rename = "type")]
    pub type_: String,
    /// Versions to mirror. If absent, all available versions are mirrored.
    pub versions: Option<Vec<String>>,
    /// Platforms to mirror. Defaults to the five most common ones.
    pub platforms: Option<Vec<Platform>>,
}

/// `POST /registry/mirror`
///
/// Fetches providers from `registry.terraform.io` and stores them locally.
pub async fn mirror_upstream(
    _auth: AuthUser,
    State(app): State<AppState>,
    Json(req): Json<MirrorRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let client = reqwest::Client::new();
    let ns = &req.namespace;
    let tp = &req.type_;

    let upstream: Value = client
        .get(format!("https://registry.terraform.io/v1/providers/{ns}/{tp}/versions"))
        .header("User-Agent", "terrarium-mirror/1.0")
        .send().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
        .error_for_status().map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
        .json().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    let all_versions: Vec<String> = upstream["versions"].as_array().unwrap_or(&vec![])
        .iter().filter_map(|v| v["version"].as_str().map(String::from)).collect();

    let want_versions: Vec<String> = match &req.versions {
        Some(r) => all_versions.into_iter().filter(|v| r.contains(v)).collect(),
        None    => all_versions,
    };

    let platforms = req.platforms.unwrap_or_else(|| vec![
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
            let Some(info) = info else { continue; };
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

    tracing::info!("🪞 Mirror {ns}/{tp}: {} ok, {} errors", mirrored.len(), errors.len());
    Ok(Json(json!({ "mirrored": mirrored, "errors": errors })))
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
    let (_host, ns, tp, rest) = (parts[0], parts[1], parts[2], parts[3]);

    // ── index.json ──
    if rest == "index.json" {
        let versions = app.registry.list_versions(ns, tp);
        if versions.is_empty() { return StatusCode::NOT_FOUND.into_response(); }
        let map: serde_json::Map<String, Value> =
            versions.into_iter().map(|v| (v, json!({}))).collect();
        return Json(json!({ "versions": Value::Object(map) })).into_response();
    }

    // ── {version}.json ──
    if let Some(ver) = rest.strip_suffix(".json") {
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
            for ver in app.registry.list_versions(ns, tp) {
                if let Some(os_arch) = suffix.strip_prefix(&format!("{ver}_")) {
                    if let Some((os, arch)) = os_arch.split_once('_') {
                        if let Some(data) = app.registry.get_zip(ns, tp, &ver, os, arch) {
                            return ([(header::CONTENT_TYPE, "application/zip")], data).into_response();
                        }
                    }
                }
            }
        }
    }

    StatusCode::NOT_FOUND.into_response()
}
