use authur::extractor::BasicAuthUser;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::body::Bytes;
use std::path::{Path as FsPath, PathBuf};

use crate::AppState;

#[derive(Clone)]
pub struct StateContainer {
    pub dir: PathBuf,
}

impl StateContainer {
    pub fn new(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        let path = self.dir.join(name);
        if path.exists() {
            Some(std::fs::read(path).unwrap())
        } else {
            None
        }
    }

    pub fn remove(&self, name: &str) {
        let _ = std::fs::remove_file(self.dir.join(name));
        let _ = std::fs::remove_file(self.dir.join(format!("{name}.archived")));
    }

    pub fn insert(&self, name: &str, state: Vec<u8>) {
        let path = self.dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, state).unwrap();
    }

    pub fn is_archived(&self, name: &str) -> bool {
        self.dir.join(format!("{name}.archived")).exists()
    }

    /// Mark a state as archived (read-only). Returns false if the state doesn't exist.
    pub fn archive(&self, name: &str) -> bool {
        if !self.dir.join(name).exists() {
            return false;
        }
        let sidecar = self.dir.join(format!("{name}.archived"));
        if let Some(parent) = sidecar.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(sidecar, b"").is_ok()
    }

    /// List state names, optionally scoped to a path prefix (e.g. "infra/").
    pub fn list(&self, prefix: Option<&str>) -> Vec<String> {
        let search_dir = match prefix {
            Some(p) => self.dir.join(p.trim_end_matches('/')),
            None => self.dir.clone(),
        };
        let mut names = Vec::new();
        self.collect_states(&search_dir, &mut names);
        names.sort();
        names
    }

    fn collect_states(&self, dir: &FsPath, names: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                self.collect_states(&path, names);
            } else {
                let file_name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                if file_name.ends_with(".archived") {
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

fn validate_name(name: &str) -> Result<(), StatusCode> {
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
    if trimmed.is_empty() {
        return Ok(());
    }
    validate_name(trimmed)
}

#[derive(serde::Deserialize)]
pub struct ListQuery {
    pub prefix: Option<String>,
}

/// List all state names, optionally scoped to a path prefix
pub async fn list_states(
    State(app): State<AppState>,
    _auth: BasicAuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<String>>, StatusCode> {
    if let Some(ref p) = q.prefix {
        validate_prefix(p)?;
    }
    Ok(Json(app.state.list(q.prefix.as_deref())))
}

/// Archive a state — marks it read-only, future pushes will be rejected
pub async fn archive_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    _auth: BasicAuthUser,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("📦 Archiving state {name}");
    if app.state.archive(&name) {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// Get the current terraform state
pub async fn get_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    _auth: BasicAuthUser,
) -> Result<Bytes, StatusCode> {
    validate_name(&name)?;
    tracing::info!("🔖 Getting state for {name}");
    match app.state.get(&name) {
        Some(data) => Ok(Bytes::from(data)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(non_snake_case)]
pub struct LockQuery {
    pub ID: Option<String>,
}

/// Update terraform state via POST
pub async fn put_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(lock): Query<LockQuery>,
    _auth: BasicAuthUser,
    body: Bytes,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("✍️ Trying to update state for {name}");

    if app.state.is_archived(&name) {
        tracing::info!("📦 State {name} is archived, rejecting write");
        return Err(StatusCode::FORBIDDEN);
    }

    if let Some(lock_id) = lock.ID {
        if !app.locks.verify_lock(&name, &lock_id) {
            return Err(StatusCode::LOCKED);
        }
    }

    app.state.insert(&name, body.to_vec());
    Ok(StatusCode::OK)
}

/// Delete terraform state via DELETE
pub async fn delete_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(lock): Query<LockQuery>,
    _auth: BasicAuthUser,
) -> Result<StatusCode, StatusCode> {
    validate_name(&name)?;
    tracing::info!("♻️ Trying to delete state for {name}");

    if let Some(lock_id) = lock.ID {
        if !app.locks.verify_lock(&name, &lock_id) {
            return Err(StatusCode::LOCKED);
        }
    }

    app.state.remove(&name);
    Ok(StatusCode::OK)
}
