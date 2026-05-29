use crate::AppState;
use crate::auth::AuthUser;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
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
        return Err(StatusCode::FORBIDDEN);
    }

    let locks = &app.locks;

    if let Some(_) = locks.get(&name) {
        tracing::info!("🔒 Already existing lock for {name}");
        return Err(StatusCode::CONFLICT);
    }

    tracing::info!("🔒 Acquired lock for {name}: {info:#?}");
    locks.insert(&name, info.clone());
    app.webhooks.fire("lock.acquire", &name, None, _auth.0.username.as_str()).await;
    Ok(Json(info))
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
        app.webhooks.fire("lock.release", &name, None, &user.username).await;
        Ok(Json(info))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
