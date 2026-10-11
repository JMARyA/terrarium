use crate::auth::AuthUser;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::body::Bytes;
use dashmap::DashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use crate::AppState;

#[derive(Clone)]
pub struct StateContainer {
    pub dir: PathBuf,
    pub versions_dir: PathBuf,
    /// Per-workspace guards; see [`StateContainer::write`].
    writers: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

/// Read a file, mapping "does not exist" to `None` and any other failure to
/// an error, so an unreadable state is never mistaken for an absent one.
fn read_opt(path: &FsPath) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

impl StateContainer {
    pub fn new(dir: PathBuf, versions_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&versions_dir).unwrap();
        Self { dir, versions_dir, writers: Arc::default() }
    }

    pub fn get(&self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        read_opt(&self.dir.join(name))
    }

    pub fn get_version(&self, name: &str, version: u32) -> std::io::Result<Option<Vec<u8>>> {
        read_opt(&self.versions_dir.join(name).join(version.to_string()))
    }

    pub fn list_versions(&self, name: &str) -> Vec<u32> {
        let mut versions: Vec<u32> = std::fs::read_dir(self.versions_dir.join(name))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter_map(|n| n.parse::<u32>().ok())
                    .collect()
            })
            .unwrap_or_default();
        versions.sort_unstable();
        versions
    }

    /// Run `f` with exclusive access to one workspace.
    ///
    /// Everything that checks a workspace and then changes it — a push
    /// verifying the lock before writing, a lock request checking the archive
    /// marker, allocating the next version number — happens inside this
    /// guard, so no other writer can slip in between the check and the write.
    /// `f` must not block on anything but the filesystem.
    pub fn write<R>(&self, name: &str, f: impl FnOnce(Writer<'_>) -> R) -> R {
        let guard = self.writers.entry(name.to_string()).or_default().clone();
        let _held = guard.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(Writer { c: self, name })
    }

    pub fn is_archived(&self, name: &str) -> bool {
        self.dir.join(format!("{name}.archived")).exists()
    }

    pub fn list(&self, prefix: Option<&str>, archived: bool) -> Vec<String> {
        let mut names = Vec::new();
        self.collect_states(&self.dir.clone(), &mut names, archived);
        if let Some(p) = prefix {
            names.retain(|n| n.starts_with(p));
        }
        names.sort();
        names
    }

    fn collect_states(&self, dir: &FsPath, names: &mut Vec<String>, archived: bool) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                self.collect_states(&path, names, archived);
            } else {
                let file_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                // Never list .archived sidecar files, or leftover atomic-write
                // temp files (`.<name>.<pid>.<seq>.tmp`) from a crashed write.
                if file_name.ends_with(".archived") || file_name.ends_with(".tmp") {
                    continue;
                }
                // Filter by archived status via sidecar presence
                let mut sidecar = path.as_os_str().to_owned();
                sidecar.push(".archived");
                let is_archived = std::path::Path::new(&sidecar).exists();
                if is_archived != archived {
                    continue;
                }
                if let Ok(rel) = path.strip_prefix(&self.dir) {
                    if let Some(s) = rel.to_str() {
                        names.push(s.to_string());
                    }
                }
            }
        }
    }
}

/// Exclusive access to one workspace, handed out by [`StateContainer::write`].
pub struct Writer<'a> {
    c: &'a StateContainer,
    name: &'a str,
}

impl Writer<'_> {
    /// Persist a new state revision.
    ///
    /// Returns an [`std::io::Error`] instead of panicking so the caller can
    /// surface a `500` and record a metric when the underlying filesystem
    /// fails (e.g. the data volume is full). Both writes are atomic (temp file
    /// + rename), so a failed or partial write can never truncate or corrupt
    /// an existing state or version file. The versioned copy is written first,
    /// so the head is only advanced once the history entry is durable.
    ///
    /// Returns the version that was written.
    pub fn insert(&self, state: &[u8]) -> std::io::Result<u32> {
        let c = self.c;
        let version = c.list_versions(self.name).last().copied().unwrap_or(0) + 1;
        let version_path = c.versions_dir.join(self.name).join(version.to_string());
        atomic_write(&version_path, state)?;

        atomic_write(&c.dir.join(self.name), state)?;
        Ok(version)
    }

    pub fn remove(&self) {
        let _ = std::fs::remove_file(self.c.dir.join(self.name));
        let _ = std::fs::remove_file(self.c.dir.join(format!("{}.archived", self.name)));
        // version history is intentionally kept
    }

    pub fn archive(&self) -> bool {
        if !self.c.dir.join(self.name).exists() {
            return false;
        }
        let sidecar = self.c.dir.join(format!("{}.archived", self.name));
        if let Some(parent) = sidecar.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(sidecar, b"").is_ok()
    }

    pub fn unarchive(&self) -> bool {
        let sidecar = self.c.dir.join(format!("{}.archived", self.name));
        if sidecar.exists() {
            std::fs::remove_file(sidecar).is_ok()
        } else {
            false
        }
    }
}

/// Write `data` to `path` atomically: create parents, write to a unique temp
/// file in the same directory, then rename over the target. The rename is
/// atomic on a single filesystem, so a reader never sees a partial file and a
/// failing write leaves any existing file untouched. The temp file is cleaned
/// up on failure.
pub(crate) fn atomic_write(path: &FsPath, data: &[u8]) -> std::io::Result<()> {
    atomic_write_mode(path, data, None)
}

/// [`atomic_write`] for files holding secrets: the file is created
/// owner-read/write only (`0600`) before any data is written, so there is no
/// window in which it is readable with the default umask.
pub(crate) fn atomic_write_private(path: &FsPath, data: &[u8]) -> std::io::Result<()> {
    atomic_write_mode(path, data, Some(0o600))
}

fn atomic_write_mode(path: &FsPath, data: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid state path"))?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{file_name}.{}.{seq}.tmp", std::process::id()));

    // A crashed writer with a recycled pid can leave this exact temp name
    // behind; clear it so `create_new` below applies `mode` to a fresh file.
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let result = options
        .open(&tmp)
        .and_then(|mut f| f.write_all(data))
        .and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn validate_name(name: &str) -> Result<(), StatusCode> {
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') {
        return Err(StatusCode::BAD_REQUEST);
    }
    for component in name.split('/') {
        if component.is_empty() || component == ".." || component == "." {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

fn validate_prefix(prefix: &str) -> Result<(), StatusCode> {
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() { return Ok(()); }
    validate_name(trimmed)
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    pub prefix: Option<String>,
    pub archived: Option<bool>,
}

#[derive(serde::Deserialize)]
pub struct GetQuery {
    pub version: Option<u32>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
pub struct LockQuery {
    pub ID: Option<String>,
}

/// List all state names, optionally scoped to a path prefix
pub async fn list_states(
    State(app): State<AppState>,
    _auth: AuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    if let Some(ref p) = q.prefix {
        validate_prefix(p)?;
    }
    Ok(Json(app.state.list(q.prefix.as_deref(), q.archived.unwrap_or(false))))
}

/// List all versions for a state
pub async fn list_versions(
    State(app): State<AppState>,
    Path(name): Path<String>,
    _auth: AuthUser,
) -> Result<Json<Vec<u32>>, StatusCode> {
    validate_name(&name)?;
    Ok(Json(app.state.list_versions(&name)))
}

/// Unarchive a state — removes the read-only marker
pub async fn unarchive_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    AuthUser(user): AuthUser,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("📬 Unarchiving state {name}");
    if app.state.write(&name, |w| w.unarchive()) {
        metrics::counter!("terrarium_state_archives_total", "workspace" => name.clone(), "action" => "unarchive", "result" => "ok").increment(1);
        app.webhooks.fire("state.unarchive", &name, None, &user.username).await;
        Ok(StatusCode::OK)
    } else {
        metrics::counter!("terrarium_state_archives_total", "workspace" => name.clone(), "action" => "unarchive", "result" => "not_found").increment(1);
        Err(StatusCode::NOT_FOUND)
    }
}

/// Archive a state — marks it read-only, rejects future pushes
pub async fn archive_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    AuthUser(user): AuthUser,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("📦 Archiving state {name}");
    if app.state.write(&name, |w| w.archive()) {
        metrics::counter!("terrarium_state_archives_total", "workspace" => name.clone(), "action" => "archive", "result" => "ok").increment(1);
        app.webhooks.fire("state.archive", &name, None, &user.username).await;
        Ok(StatusCode::OK)
    } else {
        metrics::counter!("terrarium_state_archives_total", "workspace" => name.clone(), "action" => "archive", "result" => "not_found").increment(1);
        Err(StatusCode::NOT_FOUND)
    }
}

/// Get the current terraform state, or a specific version with ?version=N
pub async fn get_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<GetQuery>,
    _auth: AuthUser,
) -> Result<Bytes, StatusCode> {
    validate_name(&name)?;
    tracing::info!("🔖 Getting state for {name}");
    let data = match q.version {
        Some(v) => app.state.get_version(&name, v),
        None => app.state.get(&name),
    };
    let data = data.map_err(|e| {
        tracing::error!("💥 Failed to read state for {name}: {e}");
        metrics::counter!("terrarium_state_pulls_total", "workspace" => name.clone(), "result" => "error").increment(1);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    match data {
        Some(data) => {
            metrics::counter!("terrarium_state_pulls_total", "workspace" => name.clone(), "result" => "ok").increment(1);
            metrics::histogram!("terrarium_state_pull_bytes", "workspace" => name.clone()).record(data.len() as f64);
            Ok(Bytes::from(data))
        }
        None => {
            metrics::counter!("terrarium_state_pulls_total", "workspace" => name.clone(), "result" => "not_found").increment(1);
            Err(StatusCode::NOT_FOUND)
        }
    }
}

/// Whether a write to `name` carrying `lock_id` may proceed.
///
/// A held lock admits only its own ID — a write with no ID or a different one
/// is refused, otherwise anyone could push over a running apply just by
/// leaving `?ID=` off. A write naming an ID when no lock is held is refused
/// too: its lock was released or taken over, so the writer is working from a
/// view of the state that may no longer be current.
fn lock_permits(app: &AppState, name: &str, lock_id: Option<&str>) -> bool {
    match lock_id {
        Some(id) => app.locks.verify_lock(name, id),
        None => app.locks.get(name).is_none(),
    }
}

/// Update terraform state via POST
pub async fn put_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(lock): Query<LockQuery>,
    AuthUser(user): AuthUser,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("✍️ Trying to update state for {name}");

    // Archive, lock and write are checked and applied under one guard, so a
    // lock acquired concurrently can't land between the check and the write.
    let outcome = app.state.write(&name, |w| {
        if app.state.is_archived(&name) {
            return Err(("forbidden", StatusCode::FORBIDDEN));
        }
        if !lock_permits(&app, &name, lock.ID.as_deref()) {
            return Err(("locked", StatusCode::LOCKED));
        }
        // Only feeds metrics, so an unreadable previous state is not fatal here.
        let previous = app.state.get(&name).unwrap_or_else(|e| {
            tracing::warn!("Failed to read previous state for {name}: {e}");
            None
        });
        match w.insert(&body) {
            Ok(version) => Ok((version, previous)),
            Err(e) => {
                tracing::error!("💥 Failed to persist state for {name}: {e}");
                metrics::counter!("terrarium_state_write_errors_total", "workspace" => name.clone()).increment(1);
                Err(("error", StatusCode::INTERNAL_SERVER_ERROR))
            }
        }
    });
    let (version, previous) = match outcome {
        Ok(ok) => ok,
        Err((result, status)) => {
            if status == StatusCode::FORBIDDEN {
                tracing::info!("📦 State {name} is archived, rejecting write");
            }
            metrics::counter!("terrarium_state_pushes_total", "workspace" => name.clone(), "result" => result).increment(1);
            return Err(status);
        }
    };
    let existed = previous.is_some();
    let before_counts = previous.as_deref().and_then(crate::observability::state_counts);
    let before_len = previous.as_ref().map(|b| b.len()).unwrap_or(0);
    let body_len = body.len();
    let after_counts = crate::observability::state_counts(&body);
    metrics::counter!("terrarium_state_pushes_total", "workspace" => name.clone(), "result" => "ok").increment(1);
    metrics::counter!("terrarium_state_version_creations_total", "workspace" => name.clone()).increment(1);
    metrics::histogram!("terrarium_state_push_bytes", "workspace" => name.clone()).record(body_len as f64);
    metrics::histogram!("terrarium_state_change_bytes", "workspace" => name.clone()).record(body_len.abs_diff(before_len) as f64);
    metrics::gauge!("terrarium_state_last_activity_timestamp_seconds", "workspace" => name.clone())
        .set(crate::observability::unix_now() as f64);
    if !existed {
        metrics::counter!("terrarium_state_creations_total", "workspace" => name.clone()).increment(1);
    }
    if let (Some((br, bi, bo)), Some((ar, ai, ao))) = (before_counts, after_counts) {
        crate::observability::observe_delta("terrarium_tf_resource_delta", &name, br, ar);
        crate::observability::observe_delta("terrarium_tf_instance_delta", &name, bi, ai);
        crate::observability::observe_delta("terrarium_tf_output_delta", &name, bo, ao);
    }
    let version = Some(version);

    // Lint the state we just stored. Deliberately after the write and after the
    // response is decided: server-side evaluation records what it finds and
    // never rejects a push, so it must not be able to delay or fail one either.
    crate::policy::spawn_state_lint(
        app.policies.clone(),
        app.violations.clone(),
        name.clone(),
        body.to_vec(),
        user.username.clone(),
        version,
    );

    app.webhooks.fire("state.push", &name, version, &user.username).await;
    Ok(StatusCode::OK)
}

/// Delete terraform state via DELETE
pub async fn delete_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(lock): Query<LockQuery>,
    AuthUser(user): AuthUser,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("♻️ Trying to delete state for {name}");

    let permitted = app.state.write(&name, |w| {
        let ok = lock_permits(&app, &name, lock.ID.as_deref());
        if ok {
            w.remove();
        }
        ok
    });
    if !permitted {
        metrics::counter!("terrarium_state_deletions_total", "workspace" => name.clone(), "result" => "locked").increment(1);
        return Err(StatusCode::LOCKED);
    }
    // A violation report describes state that no longer exists.
    app.violations.clear(&name);
    metrics::counter!("terrarium_state_deletions_total", "workspace" => name.clone(), "result" => "ok").increment(1);
    app.webhooks.fire("state.delete", &name, None, &user.username).await;
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_container() -> (StateContainer, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "terrarium-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let c = StateContainer::new(base.join("state"), base.join("versions"));
        (c, base)
    }

    #[test]
    fn insert_persists_head_and_version() {
        let (c, base) = tmp_container();
        c.write("infra/prod", |w| w.insert(b"v1")).unwrap();
        c.write("infra/prod", |w| w.insert(b"v2")).unwrap();

        assert_eq!(c.get("infra/prod").unwrap().as_deref(), Some(&b"v2"[..]));
        assert_eq!(c.list_versions("infra/prod"), vec![1, 2]);
        assert_eq!(c.get_version("infra/prod", 1).unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(c.get("missing").unwrap(), None);
        // A nested workspace must be the only listed state (no temp/sidecar noise).
        assert_eq!(c.list(None, false), vec!["infra/prod".to_string()]);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn concurrent_inserts_get_distinct_versions() {
        let (c, base) = tmp_container();
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let c = c.clone();
                std::thread::spawn(move || c.write("app", |w| w.insert(format!("v{i}").as_bytes())).unwrap())
            })
            .collect();
        let mut written: Vec<u32> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        written.sort_unstable();

        assert_eq!(written, (1..=16).collect::<Vec<_>>());
        assert_eq!(c.list_versions("app"), written);
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_state_is_an_error_not_absent() {
        let (c, base) = tmp_container();
        // A directory where the state file should be makes `read` fail with
        // something other than NotFound.
        std::fs::create_dir_all(c.dir.join("broken")).unwrap();
        assert!(c.get("broken").is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn atomic_write_leaves_no_temp_files() {
        let (c, base) = tmp_container();
        c.write("app", |w| w.insert(b"data")).unwrap();
        // No leftover ".*.tmp" files in the state dir after a successful write.
        let leftovers: Vec<_> = std::fs::read_dir(&c.dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "unexpected temp files: {leftovers:?}");
        let _ = std::fs::remove_dir_all(base);
    }
}
