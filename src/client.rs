use std::collections::HashMap;

use reqwest::Client;

use crate::lock::LockInfo;

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

    pub async fn list_states(&self, prefix: Option<&str>) -> Result<Vec<String>, String> {
        let mut url = format!("{}/state", self.base_url);
        if let Some(p) = prefix {
            url = format!("{url}?prefix={p}");
        }
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
}
