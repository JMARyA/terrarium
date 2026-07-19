use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::AppState;

static PROMETHEUS: OnceLock<PrometheusHandle> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

pub fn enabled() -> bool {
    std::env::var("TERRARIUM_METRICS")
        .ok()
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub fn init() {
    START.get_or_init(Instant::now);
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus metrics recorder");
    let _ = PROMETHEUS.set(handle);
    gauge!("terrarium_build_info", "version" => env!("CARGO_PKG_VERSION")).set(1.0);
}

pub async fn metrics(headers: HeaderMap) -> impl IntoResponse {
    match metrics_token() {
        Some(token) => {
            let ok = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h == format!("Bearer {token}"));
            if !ok {
                return (StatusCode::UNAUTHORIZED, "unauthorized\n".to_string());
            }
        }
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "metrics token is not configured\n".to_string(),
            );
        }
    }

    match PROMETHEUS.get() {
        Some(handle) => (StatusCode::OK, handle.render()),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "metrics recorder is not initialized\n".to_string(),
        ),
    }
}

fn metrics_token() -> Option<String> {
    if let Ok(path) = std::env::var("TERRARIUM_METRICS_TOKEN_FILE") {
        return std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    std::env::var("TERRARIUM_METRICS_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
}

pub async fn http_middleware(req: Request<Body>, next: Next) -> Response {
    let method = req.method().as_str().to_string();
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| scrub_path(req.uri().path()));
    if let Some(len) = content_length(req.headers()) {
        histogram!("terrarium_http_request_body_bytes", "route" => route.clone())
            .record(len as f64);
    }

    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status().as_u16().to_string();
    if let Some(len) = content_length(response.headers()) {
        histogram!("terrarium_http_response_body_bytes", "route" => route.clone())
            .record(len as f64);
    }
    let elapsed = started.elapsed().as_secs_f64();

    counter!("terrarium_http_requests_total", "method" => method.clone(), "route" => route.clone(), "status" => status.clone()).increment(1);
    histogram!("terrarium_http_request_duration_seconds", "method" => method, "route" => route, "status" => status).record(elapsed);
    response
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn scrub_path(path: &str) -> String {
    // Fallback only. MatchedPath should normally provide normalized Axum routes.
    if path == "/metrics" {
        return "/metrics".into();
    }
    if path.starts_with("/state/") {
        return "/state/{*name}".into();
    }
    if path.starts_with("/lock/") {
        return "/lock/{*name}".into();
    }
    if path.starts_with("/versions/") {
        return "/versions/{*name}".into();
    }
    if path.starts_with("/archive/") {
        return "/archive/{*name}".into();
    }
    if path.starts_with("/webhooks/") {
        return "/webhooks/{*workspace}".into();
    }
    if path.starts_with("/registry/") {
        return "/registry/...".into();
    }
    path.to_string()
}

pub fn spawn_collector(app: AppState, data_dir: PathBuf) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            collect(&app, &data_dir).await;
            ticker.tick().await;
        }
    });
}

async fn collect(app: &AppState, data_dir: &Path) {
    let active_states = app.state.list(None, false);
    let archived_states = app.state.list(None, true);
    gauge!("terrarium_states_total", "archived" => "false").set(active_states.len() as f64);
    gauge!("terrarium_states_total", "archived" => "true").set(archived_states.len() as f64);
    gauge!("terrarium_state_versions_total").set(count_files(&data_dir.join("versions")) as f64);
    for name in &active_states {
        if let Some(bytes) = app.state.get(name) {
            histogram!("terrarium_state_size_bytes").record(bytes.len() as f64);
        }
        histogram!("terrarium_state_versions_per_state")
            .record(app.state.list_versions(name).len() as f64);
    }

    let current_bytes = dir_size(&data_dir.join("state"));
    let version_bytes = dir_size(&data_dir.join("versions"));
    let lock_bytes = dir_size(&data_dir.join("locks"));
    let registry_bytes = dir_size(&data_dir.join("registry"));
    gauge!("terrarium_storage_current_state_bytes").set(current_bytes as f64);
    gauge!("terrarium_storage_state_versions_bytes").set(version_bytes as f64);
    gauge!("terrarium_storage_locks_bytes").set(lock_bytes as f64);
    gauge!("terrarium_storage_registry_bytes").set(registry_bytes as f64);
    gauge!("terrarium_storage_total_bytes").set(dir_size(data_dir) as f64);

    let locks = app.locks.list();
    gauge!("terrarium_locks_active").set(locks.len() as f64);
    gauge!("terrarium_lock_max_age_seconds").set(max_lock_age(&locks) as f64);

    gauge!("terrarium_webhooks_registered").set(app.webhooks.hooks.read().await.len() as f64);

    let (resources, managed, data, instances, outputs, sensitive_outputs) =
        tf_aggregates(&app.state, &active_states);
    gauge!("terrarium_tf_resources_total").set(resources as f64);
    gauge!("terrarium_tf_resources_by_mode_total", "mode" => "managed").set(managed as f64);
    gauge!("terrarium_tf_resources_by_mode_total", "mode" => "data").set(data as f64);
    gauge!("terrarium_tf_resource_instances_total").set(instances as f64);
    gauge!("terrarium_tf_outputs_total").set(outputs as f64);
    gauge!("terrarium_tf_sensitive_outputs_total").set(sensitive_outputs as f64);

    let providers_dir = data_dir.join("registry").join("providers");
    gauge!("terrarium_registry_providers_total")
        .set(registry_provider_count(&providers_dir) as f64);
    gauge!("terrarium_registry_versions_total").set(registry_version_count(&providers_dir) as f64);
    gauge!("terrarium_registry_platform_archives_total")
        .set(registry_zip_count(&providers_dir) as f64);

    let mirror = app.mirror_status.read().await;
    gauge!("terrarium_registry_mirror_running").set(if mirror.running { 1.0 } else { 0.0 });
    if mirror.total_errors == 0 {
        if let Some(ts) = mirror.last_sync_finished {
            gauge!("terrarium_registry_mirror_last_success_timestamp_seconds").set(ts as f64);
        }
    }

    if let Some(start) = START.get() {
        gauge!("terrarium_uptime_seconds").set(start.elapsed().as_secs_f64());
    }
}

fn tf_aggregates(
    state: &crate::state::StateContainer,
    names: &[String],
) -> (usize, usize, usize, usize, usize, usize) {
    let mut resources = 0;
    let mut managed = 0;
    let mut data = 0;
    let mut instances = 0;
    let mut outputs = 0;
    let mut sensitive_outputs = 0;

    for name in names {
        let Some(bytes) = state.get(name) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(map) = value.get("outputs").and_then(|v| v.as_object()) {
            outputs += map.len();
            sensitive_outputs += map
                .values()
                .filter(|v| {
                    v.get("sensitive")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                })
                .count();
        }
        if let Some(arr) = value.get("resources").and_then(|v| v.as_array()) {
            resources += arr.len();
            for r in arr {
                match r.get("mode").and_then(|v| v.as_str()).unwrap_or("managed") {
                    "data" => data += 1,
                    _ => managed += 1,
                }
                instances += r
                    .get("instances")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
            }
        }
    }

    (
        resources,
        managed,
        data,
        instances,
        outputs,
        sensitive_outputs,
    )
}

pub fn state_counts(bytes: &[u8]) -> Option<(usize, usize, usize)> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    let resources = value
        .get("resources")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let instances = value
        .get("resources")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    r.get("instances")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0);
    let outputs = value
        .get("outputs")
        .and_then(|v| v.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    Some((resources, instances, outputs))
}

pub fn observe_delta(metric: &'static str, before: usize, after: usize) {
    let direction = if after > before {
        "positive"
    } else if after < before {
        "negative"
    } else {
        "zero"
    };
    let delta = after.abs_diff(before) as f64;
    histogram!(metric, "direction" => direction).record(delta);
}

pub fn observe_lock_age(info: &crate::lock::LockInfo) {
    if let Some(age) = lock_age_seconds(info) {
        histogram!("terrarium_lock_age_seconds").record(age as f64);
    }
}

pub fn lock_operation(op: Option<&String>) -> &'static str {
    match op.map(|s| s.to_ascii_lowercase()) {
        Some(s) if s.contains("plan") => "plan",
        Some(s) if s.contains("apply") => "apply",
        Some(s) if s.contains("destroy") => "destroy",
        Some(s) if s.contains("refresh") => "refresh",
        Some(_) => "other",
        None => "unknown",
    }
}

fn max_lock_age(locks: &std::collections::HashMap<String, crate::lock::LockInfo>) -> u64 {
    locks
        .values()
        .filter_map(lock_age_seconds)
        .max()
        .unwrap_or(0)
}

fn lock_age_seconds(lock: &crate::lock::LockInfo) -> Option<u64> {
    let created = lock.Created.as_ref()?;
    let created = chrono::DateTime::parse_from_rfc3339(created).ok()?;
    let now = chrono::Utc::now();
    (now - created.with_timezone(&chrono::Utc))
        .to_std()
        .ok()
        .map(|d| d.as_secs())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn count_files(path: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() { count_files(&p) } else { 1 }
        })
        .sum()
}

fn registry_provider_count(path: &Path) -> usize {
    let Ok(ns) = std::fs::read_dir(path) else {
        return 0;
    };
    ns.filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|n| {
            std::fs::read_dir(n.path())
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

fn registry_version_count(path: &Path) -> usize {
    let Ok(ns) = std::fs::read_dir(path) else {
        return 0;
    };
    ns.filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|n| {
            std::fs::read_dir(n.path())
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|tp| {
                    std::fs::read_dir(tp.path())
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .filter(|e| e.path().is_dir())
                                .count()
                        })
                        .unwrap_or(0)
                })
                .sum::<usize>()
        })
        .sum()
}

fn registry_zip_count(path: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                registry_zip_count(&p)
            } else if p.extension().and_then(|e| e.to_str()) == Some("zip") {
                1
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, header};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_metrics_env() {
        unsafe {
            std::env::remove_var("TERRARIUM_METRICS_TOKEN");
            std::env::remove_var("TERRARIUM_METRICS_TOKEN_FILE");
        }
    }

    #[test]
    fn enabled_parses_truthy_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        unsafe { std::env::set_var("TERRARIUM_METRICS", "1") };
        assert!(enabled());
        unsafe { std::env::set_var("TERRARIUM_METRICS", "true") };
        assert!(enabled());
        unsafe { std::env::set_var("TERRARIUM_METRICS", "0") };
        assert!(!enabled());
        unsafe { std::env::remove_var("TERRARIUM_METRICS") };
    }

    #[test]
    fn metrics_token_prefers_token_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        let path =
            std::env::temp_dir().join(format!("terrarium-metrics-token-{}", std::process::id()));
        std::fs::write(&path, "from-file\n").unwrap();
        unsafe {
            std::env::set_var("TERRARIUM_METRICS_TOKEN", "from-env");
            std::env::set_var("TERRARIUM_METRICS_TOKEN_FILE", &path);
        }
        assert_eq!(metrics_token().as_deref(), Some("from-file"));
        let _ = std::fs::remove_file(path);
        clear_metrics_env();
    }

    #[tokio::test]
    async fn metrics_endpoint_rejects_missing_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        let response = metrics(HeaderMap::new()).await.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn metrics_endpoint_rejects_wrong_bearer_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_metrics_env();
        unsafe { std::env::set_var("TERRARIUM_METRICS_TOKEN", "secret") };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        let response = metrics(headers).await.into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        clear_metrics_env();
    }

    #[test]
    fn state_counts_are_aggregate_only() {
        let state = br#"{
          "outputs": { "url": { "sensitive": false }, "password": { "sensitive": true } },
          "resources": [
            { "mode": "managed", "instances": [{}, {}] },
            { "mode": "data", "instances": [{}] }
          ]
        }"#;
        assert_eq!(state_counts(state), Some((2, 3, 2)));
    }

    #[test]
    fn scrub_path_normalizes_sensitive_paths() {
        assert_eq!(scrub_path("/state/infra/prod"), "/state/{*name}");
        assert_eq!(scrub_path("/lock/infra/prod"), "/lock/{*name}");
        assert_eq!(
            scrub_path("/registry/providers/ns/type/1/linux/amd64/zip"),
            "/registry/..."
        );
    }
}
