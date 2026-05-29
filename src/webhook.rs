use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use crate::auth::AuthUser;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub workspace: String,
    pub url: String,
    /// Events to subscribe to. Empty = all events for this workspace.
    pub events: Vec<String>,
}

#[derive(Serialize)]
pub struct WebhookPayload {
    pub event: String,
    pub workspace: String,
    pub version: Option<u32>,
    pub user: String,
    pub timestamp: String,
}

#[derive(Clone)]
pub struct WebhookStore {
    pub hooks: Arc<RwLock<Vec<Webhook>>>,
    pub path: PathBuf,
    client: Client,
}

impl WebhookStore {
    pub fn new(path: PathBuf) -> Self {
        let hooks = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            hooks: Arc::new(RwLock::new(hooks)),
            path,
            client: Client::new(),
        }
    }

    pub async fn add(&self, webhook: Webhook) {
        self.hooks.write().await.push(webhook);
        self.persist().await;
    }

    pub async fn remove(&self, id: &str) -> bool {
        let mut hooks = self.hooks.write().await;
        let before = hooks.len();
        hooks.retain(|h| h.id != id);
        let removed = hooks.len() < before;
        drop(hooks);
        if removed {
            self.persist().await;
        }
        removed
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

    async fn persist(&self) {
        let hooks = self.hooks.read().await;
        if let Ok(json) = serde_json::to_string_pretty(&*hooks) {
            let _ = std::fs::write(&self.path, json);
        }
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
            tokio::spawn(async move {
                deliver(&client, &hook.url, &payload).await;
            });
        }
    }
}

async fn deliver(client: &Client, url: &str, payload: &WebhookPayload) {
    for attempt in 0u32..4 {
        if attempt > 0 {
            let delay = std::time::Duration::from_secs(1 << (attempt - 1));
            tokio::time::sleep(delay).await;
        }
        match client.post(url).json(payload).send().await {
            Ok(resp) if resp.status().is_success() => return,
            Ok(resp) => {
                tracing::warn!("Webhook {url} returned {} (attempt {attempt})", resp.status())
            }
            Err(e) => tracing::warn!("Webhook {url} error: {e} (attempt {attempt})"),
        }
    }
    tracing::error!("Webhook {url} failed after 4 attempts");
}

// ── API handlers ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddWebhookBody {
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
}

/// POST /webhooks/{*workspace} — register a webhook for a workspace
pub async fn add_webhook(
    State(app): State<AppState>,
    Path(workspace): Path<String>,
    _auth: AuthUser,
    Json(body): Json<AddWebhookBody>,
) -> Result<Json<Webhook>, StatusCode> {
    let hook = Webhook {
        id: uuid::Uuid::new_v4().to_string(),
        workspace,
        url: body.url,
        events: body.events,
    };
    app.webhooks.add(hook.clone()).await;
    Ok(Json(hook))
}

/// GET /webhooks/{*workspace} — list webhooks for a workspace
pub async fn list_webhooks(
    State(app): State<AppState>,
    Path(workspace): Path<String>,
    _auth: AuthUser,
) -> Json<Vec<Webhook>> {
    Json(app.webhooks.list_for(&workspace).await)
}

/// DELETE /webhooks/{id} — remove a webhook by ID
pub async fn remove_webhook(
    State(app): State<AppState>,
    Path(id): Path<String>,
    _auth: AuthUser,
) -> Result<StatusCode, StatusCode> {
    if app.webhooks.remove(&id).await {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
