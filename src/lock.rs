use crate::AppState;
use crate::auth::AuthUser;
use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    response::IntoResponse,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(non_snake_case)]
pub struct LockInfo {
    pub ID: String,
    pub Operation: Option<String>,
    pub Info: Option<String>,
    pub Who: Option<String>,
    pub Version: Option<String>,
    pub Created: Option<String>,
}

pub struct LockContainer {
    pub locks: Arc<DashMap<String, LockInfo>>,
    pub persisted: PathBuf,
}

impl Clone for LockContainer {
    fn clone(&self) -> Self {
        Self {
            locks: Arc::clone(&self.locks),
            persisted: self.persisted.clone(),
        }
    }
}

impl LockContainer {
    pub fn new(dir: PathBuf) -> Self {
        if !dir.exists() {
            std::fs::create_dir_all(&dir).unwrap();
        }
        Self {
            locks: Arc::new(DashMap::new()),
            persisted: dir,
        }
    }

    pub fn get(&self, name: &str) -> Option<LockInfo> {
        self.locks.get(name).map(|x| x.clone())
    }

    pub fn remove(&self, name: &str) -> Option<LockInfo> {
        self.locks.remove(name).map(|x| x.1)
    }

    pub fn verify_lock(&self, name: &str, lock_id: &str) -> bool {
        self.get(name).map_or(false, |info| info.ID == lock_id)
    }

    pub fn list(&self) -> HashMap<String, LockInfo> {
        self.locks
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    /// Every lock ever acquired for a workspace, newest first.
    ///
    /// Persisted `.lock` files are written on acquire and never deleted on
    /// release, so they double as an audit trail of who locked the state, for
    /// which operation, and when.
    pub fn history(&self, name: &str) -> Vec<LockInfo> {
        let dir = self.persisted.join(name);
        let mut entries: Vec<LockInfo> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|x| x == "lock"))
                    .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                    .filter_map(|s| serde_json::from_str::<LockInfo>(&s).ok())
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.Created.cmp(&a.Created));
        entries
    }

    pub fn insert(&self, name: &str, lock_info: LockInfo) {
        let name_dir = self.persisted.join(name);
        if !name_dir.exists() {
            std::fs::create_dir_all(&name_dir).unwrap();
        }

        let path = name_dir.join(format!("{}.lock", lock_info.Created.as_ref().unwrap()));
        let value = serde_json::to_string(&lock_info).unwrap();
        std::fs::write(path, value).unwrap();

        self.locks.insert(name.to_string(), lock_info);
    }
}

/// List all active locks
pub async fn list_locks(
    State(app): State<AppState>,
    _auth: AuthUser,
) -> Json<HashMap<String, LockInfo>> {
    Json(app.locks.list())
}

fn validate_name(name: &str) -> Result<(), StatusCode> {
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') || name.contains('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }
    for component in name.split('/') {
        if component.is_empty() || component == ".." || component == "." {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

/// Create a lock on state
pub async fn lock(
    _auth: AuthUser, // used below for webhook username
    State(app): State<AppState>,
    Path(name): Path<String>,
    Json(info): Json<LockInfo>,
) -> Result<Json<LockInfo>, StatusCode> {
    validate_name(&name)?;
    tracing::info!("🔒 Trying to lock {name}");

    if app.state.is_archived(&name) {
        tracing::info!("📦 State {name} is archived, rejecting lock");
        metrics::counter!("terrarium_lock_acquires_total", "result" => "forbidden", "operation" => crate::observability::lock_operation(info.Operation.as_ref())).increment(1);
        return Err(StatusCode::FORBIDDEN);
    }

    let locks = &app.locks;

    if let Some(_) = locks.get(&name) {
        tracing::info!("🔒 Already existing lock for {name}");
        metrics::counter!("terrarium_lock_acquires_total", "result" => "conflict", "operation" => crate::observability::lock_operation(info.Operation.as_ref())).increment(1);
        metrics::counter!("terrarium_lock_conflicts_total").increment(1);
        return Err(StatusCode::CONFLICT);
    }

    tracing::info!("🔒 Acquired lock for {name}: {info:#?}");
    locks.insert(&name, info.clone());
    metrics::counter!("terrarium_lock_acquires_total", "result" => "ok", "operation" => crate::observability::lock_operation(info.Operation.as_ref())).increment(1);
    app.webhooks
        .fire("lock.acquire", &name, None, _auth.0.username.as_str())
        .await;
    Ok(Json(info))
}

/// Fallback for the non-standard LOCK and UNLOCK HTTP methods that the
/// Terraform HTTP backend sends by default, so no `lock_method`/`unlock_method`
/// overrides are needed in backend configs.
pub async fn lock_method_compat(
    AuthUser(user): AuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    req: Request,
) -> impl IntoResponse {
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    match req.method().as_str() {
        "LOCK" => {
            tracing::info!("🔒 Trying to lock {name}");

            if app.state.is_archived(&name) {
                metrics::counter!("terrarium_lock_acquires_total", "result" => "forbidden", "operation" => "unknown").increment(1);
                return StatusCode::FORBIDDEN.into_response();
            }
            if app.locks.get(&name).is_some() {
                metrics::counter!("terrarium_lock_acquires_total", "result" => "conflict", "operation" => "unknown").increment(1);
                metrics::counter!("terrarium_lock_conflicts_total").increment(1);
                return StatusCode::CONFLICT.into_response();
            }

            let bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                Ok(b) => b,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            let info: LockInfo = match serde_json::from_slice(&bytes) {
                Ok(i) => i,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };

            tracing::info!("🔒 Acquired lock for {name}: {info:#?}");
            app.locks.insert(&name, info.clone());
            metrics::counter!("terrarium_lock_acquires_total", "result" => "ok", "operation" => crate::observability::lock_operation(info.Operation.as_ref())).increment(1);
            app.webhooks
                .fire("lock.acquire", &name, None, &user.username)
                .await;
            Json(info).into_response()
        }
        "UNLOCK" => {
            tracing::info!("🔓 Unlocking {name}");
            if let Some(info) = app.locks.remove(&name) {
                tracing::info!("🔓 Unlocked {name}");
                metrics::counter!("terrarium_lock_releases_total", "result" => "ok").increment(1);
                crate::observability::observe_lock_age(&info);
                app.webhooks
                    .fire("lock.release", &name, None, &user.username)
                    .await;
                Json(info).into_response()
            } else {
                metrics::counter!("terrarium_lock_releases_total", "result" => "not_found")
                    .increment(1);
                StatusCode::NOT_FOUND.into_response()
            }
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// Unlock a state
pub async fn unlock(
    AuthUser(user): AuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<LockInfo>, StatusCode> {
    validate_name(&name)?;
    tracing::info!("🔓 Unlocking {name}");
    let locks = &app.locks;

    if let Some(info) = locks.remove(&name) {
        tracing::info!("🔓 Unlocked {name}");
        metrics::counter!("terrarium_lock_releases_total", "result" => "ok").increment(1);
        crate::observability::observe_lock_age(&info);
        app.webhooks
            .fire("lock.release", &name, None, &user.username)
            .await;
        Ok(Json(info))
    } else {
        metrics::counter!("terrarium_lock_releases_total", "result" => "not_found").increment(1);
        Err(StatusCode::NOT_FOUND)
    }
}
