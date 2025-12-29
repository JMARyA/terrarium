use crate::AppState;
use authur::extractor::BasicAuthUser;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

#[derive(Clone)]
pub struct LockContainer {
    pub locks: DashMap<String, LockInfo>,
    pub persisted: PathBuf,
}

impl LockContainer {
    pub fn new(dir: PathBuf) -> Self {
        if !dir.exists() {
            std::fs::create_dir_all(&dir).unwrap();
        }
        Self {
            locks: DashMap::new(),
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

/// Create a lock on state
pub async fn lock(
    _auth: BasicAuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    Json(info): Json<LockInfo>,
) -> Result<Json<LockInfo>, StatusCode> {
    tracing::info!("🔒 Trying to lock {name}");

    let locks = &app.locks;

    if let Some(_) = locks.get(&name) {
        tracing::info!("🔒 Already existing lock for {name}");
        return Err(StatusCode::CONFLICT);
    }

    tracing::info!("🔒 Acquired lock for {name}: {info:#?}");
    locks.insert(&name, info.clone());
    Ok(Json(info))
}

/// Unlock a state
pub async fn unlock(
    _auth: BasicAuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<LockInfo>, StatusCode> {
    tracing::info!("🔓 Unlocking {name}");
    let locks = &app.locks;

    if let Some(info) = locks.remove(&name) {
        tracing::info!("🔓 Unlocked {name}");
        Ok(Json(info))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
