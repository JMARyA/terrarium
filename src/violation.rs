//! Current policy-violation status, one report per workspace.
//!
//! This is deliberately a *snapshot*, not a history: the last push's lint
//! result, overwritten on every push and cleared when a workspace comes back
//! clean. It answers "is anything wrong right now", which is the question an
//! operator opens the dashboard to ask.
//!
//! Storage mirrors [`crate::lock::LockContainer`]: a hot map plus one file per
//! workspace, so the working set is bounded by workspace count and there is no
//! retention or rotation policy to get wrong.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::AuthUser;
use crate::policy::{Severity, Violation};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationReport {
    pub workspace: String,
    pub version: Option<u32>,
    /// RFC3339 timestamp of the evaluation.
    pub checked: String,
    /// Who pushed the state that was evaluated.
    pub user: String,
    pub violations: Vec<Violation>,
    /// Set when evaluation could not complete (timeout, oversized state).
    /// Rendered as its own UI state — a workspace we failed to check has not
    /// passed, and showing it as clean would be a lie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ViolationReport {
    pub fn deny_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Deny)
            .count()
    }

    pub fn warn_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Warn)
            .count()
    }

    /// Nothing to report — no violations and no failure to evaluate.
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty() && self.error.is_none()
    }
}

#[derive(Clone)]
pub struct ViolationStore {
    reports: Arc<DashMap<String, ViolationReport>>,
    dir: PathBuf,
}

impl ViolationStore {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let store = Self {
            reports: Arc::new(DashMap::new()),
            dir,
        };
        store.load();
        store
    }

    fn path_for(&self, workspace: &str) -> PathBuf {
        // Workspace names contain slashes (`infra/prod`), so mirror them as
        // directories and suffix the leaf, exactly as the lock store does.
        self.dir.join(format!("{workspace}.json"))
    }

    fn load(&self) {
        fn walk(dir: &std::path::Path, out: &mut Vec<ViolationReport>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|x| x == "json")
                    && let Ok(text) = std::fs::read_to_string(&path)
                    && let Ok(report) = serde_json::from_str::<ViolationReport>(&text)
                {
                    out.push(report);
                }
            }
        }

        let mut found = Vec::new();
        walk(&self.dir, &mut found);
        for report in found {
            self.reports.insert(report.workspace.clone(), report);
        }
    }

    pub fn get(&self, workspace: &str) -> Option<ViolationReport> {
        self.reports.get(workspace).map(|r| r.clone())
    }

    /// Every workspace currently in violation, worst first.
    pub fn list(&self) -> Vec<ViolationReport> {
        let mut all: Vec<ViolationReport> = self.reports.iter().map(|r| r.clone()).collect();
        all.sort_by(|a, b| {
            b.deny_count()
                .cmp(&a.deny_count())
                .then_with(|| b.warn_count().cmp(&a.warn_count()))
                .then_with(|| a.workspace.cmp(&b.workspace))
        });
        all
    }

    pub fn total(&self) -> usize {
        self.reports.len()
    }

    /// Record a report, or clear the workspace when the report is clean.
    ///
    /// Clearing on a clean result is what keeps a badge from outliving the
    /// problem it describes.
    pub fn record(&self, report: ViolationReport) {
        if report.is_clean() {
            self.clear(&report.workspace);
            return;
        }
        let path = self.path_for(&report.workspace);
        match serde_json::to_vec_pretty(&report) {
            Ok(bytes) => {
                if let Err(e) = crate::state::atomic_write(&path, &bytes) {
                    // The in-memory report is still authoritative for this
                    // process; losing it on restart beats failing a push.
                    tracing::warn!(
                        "Failed to persist violation report for {}: {e}",
                        report.workspace
                    );
                }
            }
            Err(e) => tracing::warn!("Failed to encode violation report: {e}"),
        }
        self.reports.insert(report.workspace.clone(), report);
    }

    pub fn clear(&self, workspace: &str) {
        self.reports.remove(workspace);
        let _ = std::fs::remove_file(self.path_for(workspace));
    }
}

// ── API handlers ─────────────────────────────────────────────────────────────

/// GET /violations — every workspace currently in violation.
pub async fn list_violations(
    State(app): State<AppState>,
    _auth: AuthUser,
) -> Json<Vec<ViolationReport>> {
    Json(app.violations.list())
}

/// GET /violations/{*workspace} — current report, 404 when clean.
pub async fn get_violations(
    State(app): State<AppState>,
    Path(workspace): Path<String>,
    _auth: AuthUser,
) -> Result<Json<ViolationReport>, StatusCode> {
    app.violations
        .get(&workspace)
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Origin;

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "terrarium-violation-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn report(workspace: &str, messages: &[(&str, Severity)]) -> ViolationReport {
        ViolationReport {
            workspace: workspace.to_string(),
            version: Some(1),
            checked: "2026-07-29T00:00:00Z".to_string(),
            user: "alice".to_string(),
            violations: messages
                .iter()
                .map(|(m, s)| Violation {
                    policy: "p".to_string(),
                    origin: Origin::Api,
                    severity: *s,
                    message: (*m).to_string(),
                })
                .collect(),
            error: None,
        }
    }

    #[test]
    fn record_read_and_clear() {
        let dir = tmp_dir();
        let store = ViolationStore::new(dir.clone());

        store.record(report("infra/prod", &[("bad", Severity::Deny)]));
        assert_eq!(store.get("infra/prod").unwrap().deny_count(), 1);
        assert_eq!(store.total(), 1);

        store.clear("infra/prod");
        assert!(store.get("infra/prod").is_none());
        assert_eq!(store.total(), 0);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_clean_report_clears_rather_than_stores() {
        // Otherwise a fixed workspace keeps its badge forever.
        let dir = tmp_dir();
        let store = ViolationStore::new(dir.clone());

        store.record(report("infra/prod", &[("bad", Severity::Deny)]));
        store.record(report("infra/prod", &[]));

        assert!(store.get("infra/prod").is_none());
        assert!(!store.path_for("infra/prod").exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_eval_error_is_not_clean() {
        let dir = tmp_dir();
        let store = ViolationStore::new(dir.clone());

        let mut r = report("infra/prod", &[]);
        r.error = Some("timed out".to_string());
        store.record(r);

        // A workspace we failed to check must not look like one that passed.
        assert!(store.get("infra/prod").is_some());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reports_survive_reload_including_nested_workspaces() {
        let dir = tmp_dir();
        let store = ViolationStore::new(dir.clone());
        store.record(report("infra/prod/db", &[("bad", Severity::Deny)]));
        store.record(report("apps", &[("meh", Severity::Warn)]));

        let reopened = ViolationStore::new(dir.clone());
        assert_eq!(reopened.total(), 2);
        assert_eq!(reopened.get("infra/prod/db").unwrap().deny_count(), 1);
        assert_eq!(reopened.get("apps").unwrap().warn_count(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn listing_puts_the_worst_workspaces_first() {
        let dir = tmp_dir();
        let store = ViolationStore::new(dir.clone());
        store.record(report("warned", &[("w", Severity::Warn)]));
        store.record(report(
            "denied",
            &[("d", Severity::Deny), ("d2", Severity::Deny)],
        ));
        store.record(report("one-deny", &[("d", Severity::Deny)]));

        let names: Vec<String> = store.list().into_iter().map(|r| r.workspace).collect();
        assert_eq!(names, vec!["denied", "one-deny", "warned"]);

        let _ = std::fs::remove_dir_all(dir);
    }
}
