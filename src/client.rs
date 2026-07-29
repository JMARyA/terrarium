use std::collections::HashMap;

use reqwest::Client;

use crate::lock::LockInfo;
use crate::policy::{Bundle, ConfigEntry, Policy};
use crate::webhook::Webhook;

/// Why fetching a policy bundle failed.
///
/// The client reacts differently to each (§11): a server that was demonstrably
/// up seconds ago going quiet is treated as an anomaly worth stopping for, while
/// a server that simply predates the feature must not brick an apply.
#[derive(Debug)]
pub enum BundleError {
    /// Transport failure — refused, DNS, TLS, timeout.
    Unreachable(String),
    /// Credentials rejected.
    Unauthorized,
    /// No policy endpoint — the server is older than this client.
    NotSupported,
    Server(String),
}

/// Percent-encode a query-string value. Workspace names are path-like
/// (`infra/prod`), so the separator and the usual reserved characters have to
/// survive the round-trip.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "server unreachable: {e}"),
            Self::Unauthorized => write!(f, "authentication rejected"),
            Self::NotSupported => write!(f, "server does not support policies"),
            Self::Server(e) => write!(f, "{e}"),
        }
    }
}

pub struct TerrariumClient {
    client: Client,
    base_url: String,
    username: String,
    password: String,
}

impl TerrariumClient {
    pub fn new(base_url: String, username: String, password: String) -> Self {
        Self {
            client: Client::new(),
            base_url,
            username,
            password,
        }
    }

    pub async fn list_states(&self, prefix: Option<&str>, archived: bool) -> Result<Vec<String>, String> {
        let mut params: Vec<String> = Vec::new();
        if let Some(p) = prefix {
            params.push(format!("prefix={p}"));
        }
        if archived {
            params.push("archived=true".to_string());
        }
        let url = if params.is_empty() {
            format!("{}/state", self.base_url)
        } else {
            format!("{}/state?{}", self.base_url, params.join("&"))
        };
        let resp = self
            .client
            .get(url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<Vec<String>>().await.map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }

    pub async fn unarchive_state(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .delete(format!("{}/archive/{name}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(format!("State '{name}' not found or not archived")),
            s => Err(format!("Server returned {s}")),
        }
    }

    pub async fn archive_state(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .post(format!("{}/archive/{name}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(format!("State '{name}' not found")),
            s => Err(format!("Server returned {s}")),
        }
    }

    pub async fn get_state(&self, name: &str, version: Option<u32>) -> Result<bytes::Bytes, String> {
        let url = match version {
            Some(v) => format!("{}/state/{name}?version={v}", self.base_url),
            None => format!("{}/state/{name}", self.base_url),
        };
        let resp = self
            .client
            .get(url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => resp.bytes().await.map_err(|e| e.to_string()),
            reqwest::StatusCode::NOT_FOUND => Err(format!("State '{name}' not found")),
            s => Err(format!("Server returned {s}")),
        }
    }

    pub async fn list_versions(&self, name: &str) -> Result<Vec<u32>, String> {
        let resp = self
            .client
            .get(format!("{}/versions/{name}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<Vec<u32>>().await.map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }

    pub async fn unlock_state(&self, name: &str) -> Result<LockInfo, String> {
        let resp = self
            .client
            .delete(format!("{}/lock/{name}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => resp.json::<LockInfo>().await.map_err(|e| e.to_string()),
            reqwest::StatusCode::NOT_FOUND => Err(format!("No lock held on '{name}'")),
            s => Err(format!("Server returned {s}")),
        }
    }

    pub async fn list_locks(&self) -> Result<HashMap<String, LockInfo>, String> {
        let resp = self
            .client
            .get(format!("{}/lock", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<HashMap<String, LockInfo>>()
                .await
                .map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }

    pub async fn add_webhook(
        &self,
        workspace: &str,
        url: &str,
        events: Vec<String>,
    ) -> Result<Webhook, String> {
        let body = serde_json::json!({ "url": url, "events": events });
        let resp = self
            .client
            .post(format!("{}/webhooks/{workspace}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<Webhook>().await.map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }

    pub async fn list_webhooks(&self, workspace: &str) -> Result<Vec<Webhook>, String> {
        let resp = self
            .client
            .get(format!("{}/webhooks/{workspace}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<Vec<Webhook>>().await.map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }

    pub async fn remove_webhook(&self, id: &str) -> Result<(), String> {
        let resp = self
            .client
            .delete(format!("{}/webhooks/id/{id}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(format!("Webhook '{id}' not found")),
            s => Err(format!("Server returned {s}")),
        }
    }

    pub async fn change_password(&self, new_password: &str) -> Result<(), String> {
        let body = serde_json::json!({
            "current_password": self.password,
            "new_password": new_password,
        });

        let resp = self
            .client
            .put(format!("{}/user/password", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::UNAUTHORIZED => Err("Incorrect current password".to_string()),
            s => Err(format!("Server returned {s}")),
        }
    }

    // ── Policies ─────────────────────────────────────────────────────────────

    /// Fetch the policies applicable to a workspace, with their source and the
    /// effective config, in one round-trip.
    pub async fn policy_bundle(&self, workspace: &str) -> Result<Bundle, BundleError> {
        let resp = self
            .client
            .get(format!(
                "{}/policy/bundle?workspace={}",
                self.base_url,
                urlencode(workspace)
            ))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| BundleError::Unreachable(e.to_string()))?;

        match resp.status() {
            s if s.is_success() => resp
                .json::<Bundle>()
                .await
                .map_err(|e| BundleError::Server(e.to_string())),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                Err(BundleError::Unauthorized)
            }
            reqwest::StatusCode::NOT_FOUND => Err(BundleError::NotSupported),
            s => Err(BundleError::Server(format!("server returned {s}"))),
        }
    }

    pub async fn list_policies(&self) -> Result<Vec<Policy>, String> {
        let resp = self
            .client
            .get(format!("{}/policy", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<Vec<Policy>>().await.map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }

    pub async fn put_policy(
        &self,
        name: &str,
        source: &str,
        workspace: &str,
        enabled: bool,
    ) -> Result<(), String> {
        let body = serde_json::json!({
            "source": source,
            "workspace": workspace,
            "enabled": enabled,
        });

        let resp = self
            .client
            .put(format!("{}/policy/{name}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            // The server explains compile failures precisely; passing its body
            // through is far more useful than the status code alone.
            let detail = resp.text().await.unwrap_or_default();
            Err(if detail.is_empty() {
                format!("Server returned {status}")
            } else {
                detail
            })
        }
    }

    pub async fn remove_policy(&self, name: &str) -> Result<(), String> {
        let resp = self
            .client
            .delete(format!("{}/policy/{name}", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(format!("Policy '{name}' not found")),
            _ => Err(resp.text().await.unwrap_or_else(|e| e.to_string())),
        }
    }

    pub async fn policy_config(&self) -> Result<Vec<ConfigEntry>, String> {
        let resp = self
            .client
            .get(format!("{}/policy/config", self.base_url))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            resp.json::<Vec<ConfigEntry>>()
                .await
                .map_err(|e| e.to_string())
        } else {
            Err(format!("Server returned {}", resp.status()))
        }
    }
}
