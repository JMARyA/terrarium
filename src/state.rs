use authur::extractor::BasicAuthUser;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{body::Bytes, extract::Query};
use std::path::PathBuf;

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
        let path = self.dir.join(name);
        let _ = std::fs::remove_file(path);
    }

    pub fn insert(&self, name: &str, state: Vec<u8>) {
        let path = self.dir.join(name);
        std::fs::write(path, state).unwrap();
    }
}

/// Get the current terraform state
/// State will be fetched via GET by Terraform.
pub async fn get_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    _auth: BasicAuthUser,
) -> Result<Bytes, StatusCode> {
    tracing::info!("🔖 Getting state for {name}");

    let states = &app.state;
    match states.get(&name) {
        Some(data) => Ok(Bytes::from(data.clone())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Debug, serde::Deserialize)]
#[allow(non_snake_case)]
pub struct LockQuery {
    /// ID of the holding lock
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
    tracing::info!("✍️ Trying to update state for {name}");

    if let Some(lock_id) = lock.ID {
        if !app.locks.verify_lock(&name, &lock_id) {
            return Err(StatusCode::LOCKED);
        }
    }

    let states = &app.state;

    states.insert(&name, body.to_vec());
    Ok(StatusCode::OK)
}

/// Delete terraform state via DELETE
pub async fn delete_state(
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(lock): Query<LockQuery>,
    _auth: BasicAuthUser,
) -> Result<StatusCode, StatusCode> {
    tracing::info!("♻️ Trying to delete state for {name}");

    if let Some(lock_id) = lock.ID {
        if !app.locks.verify_lock(&name, &lock_id) {
            return Err(StatusCode::LOCKED);
        }
    }

    let states = &app.state;
    states.remove(&name);
    Ok(StatusCode::OK)
}
