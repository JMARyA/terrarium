//! Rego policy storage and evaluation.
//!
//! Policies are Rego (OPA) documents evaluated at two sites: against Terraform
//! *state* when it is pushed to the server, and against a *plan* before `terra
//! apply` runs it. The two are distinguished by the package a policy declares —
//! `terrarium.state` or `terrarium.plan` — so a policy is only ever handed the
//! input shape it was written for.
//!
//! See `docs/policy-engine.md` for the design, including why server-side
//! evaluation never rejects a push.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use regorus::{Engine, Value};
use serde::{Deserialize, Serialize};

/// Wall-clock ceiling for a single policy evaluation.
///
/// Policies arrive from an uploader or a cloned repository, so evaluation is
/// hostile input: a combinatorial rule can burn CPU indefinitely. Rego forbids
/// unbounded recursion and cannot touch the filesystem, network, or processes,
/// so the exposure is availability only — but on the server that is a
/// push-lint stall and on the client a hung apply, both worth preventing.
fn eval_timeout() -> Duration {
    let ms = std::env::var("TERRARIUM_POLICY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2000);
    Duration::from_millis(ms)
}

/// How often the interpreter checks the clock, in work units. Checking every
/// unit is exact but pays for a clock read constantly; 64 keeps overshoot far
/// below the limits we care about.
const TIMER_CHECK_INTERVAL: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Blocks an apply when the effective mode is `enforce`.
    Deny,
    /// Never blocks, anywhere.
    Warn,
}

impl Severity {
    const fn rule(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Warn => "warn",
        }
    }
}

/// Where a policy came from. Visible in listings and in every violation
/// message, so "why can't I delete this policy" answers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Uploaded through the API (`terra policy push`).
    Api,
    /// Placed directly in the policy directory by an operator.
    File,
    /// Found in a repository's `.terrarium/policies/` — client-side only.
    Local,
}

/// Which input document a policy is written against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Site {
    State,
    Plan,
}

impl Site {
    const fn package(self) -> &'static str {
        match self {
            Self::State => "data.terrarium.state",
            Self::Plan => "data.terrarium.plan",
        }
    }

    fn from_package(pkg: &str) -> Option<Self> {
        match pkg {
            "data.terrarium.state" => Some(Self::State),
            "data.terrarium.plan" => Some(Self::Plan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub name: String,
    /// Scope. Empty applies everywhere; a trailing slash is a path prefix;
    /// anything else is an exact workspace name.
    #[serde(default)]
    pub workspace: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub origin: Origin,
    /// Sites this policy declares packages for. Derived at compile time, so a
    /// state policy is never handed a plan document.
    #[serde(default)]
    pub sites: Vec<Site>,
    /// SHA-256 of the source. Load-bearing twice: it keys the compile cache and
    /// it is what drift detection compares between a repo and the server.
    pub content_hash: String,
    pub updated: String,
    pub updated_by: String,
}

const fn default_true() -> bool {
    true
}

/// A single rule firing, attributed to the policy that produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Violation {
    pub policy: String,
    pub origin: Origin,
    pub severity: Severity,
    pub message: String,
}

/// Result of evaluating every applicable policy for one workspace.
///
/// Evaluation errors are kept separate from violations: a policy that failed to
/// run has *not* passed, and reporting it as clean would be a lie.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outcome {
    pub violations: Vec<Violation>,
    /// `(policy name, error)` for policies that could not be evaluated.
    pub errors: Vec<(String, String)>,
    pub evaluated: usize,
}

impl Outcome {
    pub fn denied(&self) -> impl Iterator<Item = &Violation> {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Deny)
    }

    pub fn is_clean(&self) -> bool {
        self.violations.is_empty() && self.errors.is_empty()
    }
}

// ── Compilation ──────────────────────────────────────────────────────────────

/// Compile one policy into a bounded, ready-to-clone engine.
///
/// Returns the engine plus the sites it declares. Rejects a policy whose
/// package is not one Terrarium evaluates: a typo like `terrarium.plans` would
/// otherwise store cleanly, run never, and silently protect nothing.
pub fn compile(name: &str, source: &str) -> Result<(Engine, Vec<Site>), String> {
    compile_with_timeout(name, source, eval_timeout())
}

/// As [`compile`], with an explicit evaluation ceiling.
///
/// The limit is bound into the engine at compile time and carried by every
/// clone, so it cannot be forgotten at an evaluation site.
pub fn compile_with_timeout(
    name: &str,
    source: &str,
    timeout: Duration,
) -> Result<(Engine, Vec<Site>), String> {
    let mut engine = Engine::new();
    // Pin the language version rather than inheriting whatever the crate
    // defaults to, so a regorus upgrade cannot silently change how stored
    // policies parse.
    engine.set_rego_v0(false);
    engine.set_execution_timer_config(regorus::utils::limits::ExecutionTimerConfig {
        limit: timeout,
        check_interval: std::num::NonZeroU32::new(TIMER_CHECK_INTERVAL).expect("nonzero"),
    });

    engine
        .add_policy(format!("{name}.rego"), source.to_string())
        .map_err(|e| e.to_string())?;

    let packages = engine.get_packages().map_err(|e| e.to_string())?;
    let sites: Vec<Site> = packages
        .iter()
        .filter_map(|p| Site::from_package(p))
        .collect();

    if sites.is_empty() {
        return Err(format!(
            "policy declares {packages:?}, but Terrarium only evaluates \
             `package terrarium.state` and `package terrarium.plan`"
        ));
    }

    Ok((engine, sites))
}

/// Evaluate one prepared engine, collecting `deny` and `warn` messages.
///
/// Uses `eval_query` rather than `eval_rule` deliberately: `eval_rule` treats an
/// absent rule as an error, and most policies define `deny` without `warn`.
fn eval_one(
    engine: &mut Engine,
    site: Site,
    input: &Value,
    policy: &str,
    origin: Origin,
) -> Result<Vec<Violation>, String> {
    engine.set_input(input.clone());

    let mut out = Vec::new();
    for severity in [Severity::Deny, Severity::Warn] {
        let query = format!("{}.{}", site.package(), severity.rule());
        let results = engine.eval_query(query, false).map_err(|e| e.to_string())?;

        let Some(expr) = results.result.first().and_then(|r| r.expressions.first()) else {
            continue; // rule absent, or no match
        };

        match &expr.value {
            Value::Set(items) => {
                for item in items.iter() {
                    let message = match item.as_string() {
                        Ok(s) => s.to_string(),
                        Err(_) => item.to_string(),
                    };
                    out.push(Violation {
                        policy: policy.to_string(),
                        origin,
                        severity,
                        message,
                    });
                }
            }
            Value::Undefined => {}
            other => {
                return Err(format!(
                    "`{}` must be a set of strings, got {other:?}",
                    severity.rule()
                ));
            }
        }
    }
    Ok(out)
}

// ── Store ────────────────────────────────────────────────────────────────────

struct Inner {
    policies: Vec<Policy>,
    /// Source keyed by policy name, kept in memory to serve bundles.
    sources: HashMap<String, String>,
    /// Compiled engines keyed by **content hash**, not name: an identical
    /// re-push or a rename reuses the artifact, and a same-name-different-source
    /// write can never serve a stale engine.
    compiled: HashMap<String, Engine>,
}

#[derive(Clone)]
pub struct PolicyStore {
    inner: Arc<RwLock<Inner>>,
    dir: PathBuf,
    timeout: Duration,
}

impl PolicyStore {
    /// Load every policy from `dir`, compiling as we go.
    ///
    /// A policy that fails to compile is kept out of the working set and logged
    /// rather than aborting startup — a bad hand-edited file should not stop the
    /// server from serving state.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_timeout(dir, eval_timeout())
    }

    /// As [`PolicyStore::new`], with an explicit per-evaluation ceiling.
    pub fn with_timeout(dir: PathBuf, timeout: Duration) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let store = Self {
            inner: Arc::new(RwLock::new(Inner {
                policies: Vec::new(),
                sources: HashMap::new(),
                compiled: HashMap::new(),
            })),
            dir,
            timeout,
        };
        store.reload();
        store
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join("policies.json")
    }

    fn source_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.rego"))
    }

    /// Rebuild the in-memory working set from disk.
    ///
    /// Metadata drives the list, but any `.rego` file present without a metadata
    /// entry is adopted as `origin: file` — that is what makes a volume-mounted
    /// policy directory work with no API calls at all.
    ///
    /// Corollary worth knowing: if `policies.json` is lost, previously
    /// API-managed policies come back as file-owned and can no longer be edited
    /// or deleted through the API. They still evaluate — nothing silently stops
    /// protecting anything — and deleting the `.rego` file remains the way out.
    pub fn reload(&self) {
        let meta: Vec<Policy> = std::fs::read_to_string(self.meta_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let mut policies: Vec<Policy> = Vec::new();
        let mut sources: HashMap<String, String> = HashMap::new();
        let mut compiled: HashMap<String, Engine> = HashMap::new();

        let timeout = self.timeout;
        let mut adopt = |name: String, source: String, known: Option<&Policy>| {
            let hash = hash_source(&source);
            match compile_with_timeout(&name, &source, timeout) {
                Ok((engine, sites)) => {
                    let policy = match known {
                        Some(p) => Policy {
                            content_hash: hash.clone(),
                            sites,
                            ..p.clone()
                        },
                        None => Policy {
                            name: name.clone(),
                            workspace: String::new(),
                            enabled: true,
                            origin: Origin::File,
                            sites,
                            content_hash: hash.clone(),
                            updated: now_rfc3339(),
                            updated_by: "file".to_string(),
                        },
                    };
                    compiled.insert(hash, engine);
                    sources.insert(name, source);
                    policies.push(policy);
                }
                Err(e) => {
                    tracing::error!("🚫 Policy {name} failed to compile, skipping: {e}");
                }
            }
        };

        let entries = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let path = e.path();
                        if path.extension().is_some_and(|x| x == "rego") {
                            let name = path.file_stem()?.to_str()?.to_string();
                            let source = std::fs::read_to_string(&path).ok()?;
                            Some((name, source))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for (name, source) in entries {
            let known = meta.iter().find(|p| p.name == name);
            adopt(name, source, known);
        }

        policies.sort_by(|a, b| a.name.cmp(&b.name));

        let mut inner = self.inner.write().expect("policy store poisoned");
        inner.policies = policies;
        inner.sources = sources;
        inner.compiled = compiled;
    }

    pub fn list(&self) -> Vec<Policy> {
        self.inner
            .read()
            .expect("policy store poisoned")
            .policies
            .clone()
    }

    pub fn source(&self, name: &str) -> Option<String> {
        self.inner
            .read()
            .expect("policy store poisoned")
            .sources
            .get(name)
            .cloned()
    }

    /// Policies applicable to a workspace at a given site, enabled only.
    pub fn applicable(&self, workspace: &str, site: Site) -> Vec<Policy> {
        self.inner
            .read()
            .expect("policy store poisoned")
            .policies
            .iter()
            .filter(|p| {
                p.enabled && p.sites.contains(&site) && scope_matches(&p.workspace, workspace)
            })
            .cloned()
            .collect()
    }

    /// Create or replace a policy.
    ///
    /// Compiles before persisting, so a syntactically broken policy is rejected
    /// at the point someone can still fix it rather than at the next push.
    /// Refuses to overwrite a file-origin policy: the file is the operator's
    /// declared intent, and shadowing it would make the UI lie.
    pub fn put(
        &self,
        name: &str,
        source: &str,
        workspace: &str,
        enabled: bool,
        user: &str,
    ) -> Result<Policy, PutError> {
        validate_name(name).map_err(PutError::Invalid)?;

        if let Some(existing) = self.list().iter().find(|p| p.name == name) {
            if existing.origin == Origin::File {
                return Err(PutError::FileOwned);
            }
        }

        let (engine, sites) =
            compile_with_timeout(name, source, self.timeout).map_err(PutError::Compile)?;
        let hash = hash_source(source);

        let policy = Policy {
            name: name.to_string(),
            workspace: workspace.to_string(),
            enabled,
            origin: Origin::Api,
            sites,
            content_hash: hash.clone(),
            updated: now_rfc3339(),
            updated_by: user.to_string(),
        };

        // Source file first, then metadata: metadata naming a policy whose
        // source is missing is the worse of the two failure modes.
        atomic_write(&self.source_path(name), source.as_bytes())
            .map_err(|e| PutError::Io(e.to_string()))?;

        {
            let mut inner = self.inner.write().expect("policy store poisoned");
            inner.policies.retain(|p| p.name != name);
            inner.policies.push(policy.clone());
            inner.policies.sort_by(|a, b| a.name.cmp(&b.name));
            inner.sources.insert(name.to_string(), source.to_string());
            inner.compiled.insert(hash, engine);
            prune_compiled(&mut inner);
        }

        self.persist_meta()
            .map_err(|e| PutError::Io(e.to_string()))?;
        Ok(policy)
    }

    pub fn remove(&self, name: &str) -> Result<bool, PutError> {
        validate_name(name).map_err(PutError::Invalid)?;

        {
            let inner = self.inner.read().expect("policy store poisoned");
            match inner.policies.iter().find(|p| p.name == name) {
                None => return Ok(false),
                Some(p) if p.origin == Origin::File => return Err(PutError::FileOwned),
                Some(_) => {}
            }
        }

        let _ = std::fs::remove_file(self.source_path(name));
        {
            let mut inner = self.inner.write().expect("policy store poisoned");
            inner.policies.retain(|p| p.name != name);
            inner.sources.remove(name);
            prune_compiled(&mut inner);
        }
        self.persist_meta()
            .map_err(|e| PutError::Io(e.to_string()))?;
        Ok(true)
    }

    /// Evaluate every applicable policy against `input`.
    ///
    /// Engines are cloned out under a short read lock and evaluated outside it,
    /// so a slow policy never blocks a write to the store. Each policy runs in
    /// its own engine: a merged one would be marginally faster but would return
    /// an undifferentiated message set, losing the attribution the UI and CLI
    /// both need.
    pub fn evaluate(&self, workspace: &str, site: Site, input: &Value) -> Outcome {
        let prepared: Vec<(Policy, Option<Engine>)> = {
            let inner = self.inner.read().expect("policy store poisoned");
            inner
                .policies
                .iter()
                .filter(|p| {
                    p.enabled && p.sites.contains(&site) && scope_matches(&p.workspace, workspace)
                })
                .map(|p| (p.clone(), inner.compiled.get(&p.content_hash).cloned()))
                .collect()
        };

        let mut outcome = Outcome {
            evaluated: prepared.len(),
            ..Default::default()
        };

        for (policy, engine) in prepared {
            let Some(mut engine) = engine else {
                // Only reachable if the cache and the working set disagree,
                // which would be a bug — surface it rather than pass silently.
                outcome
                    .errors
                    .push((policy.name.clone(), "no compiled engine".to_string()));
                continue;
            };
            match eval_one(&mut engine, site, input, &policy.name, policy.origin) {
                Ok(mut v) => outcome.violations.append(&mut v),
                Err(e) => outcome.errors.push((policy.name.clone(), e)),
            }
        }

        outcome
    }

    fn persist_meta(&self) -> std::io::Result<()> {
        let policies = self.list();
        // File-origin policies are described by the files themselves; writing
        // them into metadata would resurrect them as ghosts after the file is
        // deleted.
        let owned: Vec<&Policy> = policies
            .iter()
            .filter(|p| p.origin != Origin::File)
            .collect();
        let json = serde_json::to_vec_pretty(&owned)?;
        atomic_write(&self.meta_path(), &json)
    }
}

#[derive(Debug)]
pub enum PutError {
    Invalid(String),
    Compile(String),
    /// The name belongs to a policy owned by a file on disk.
    FileOwned,
    Io(String),
}

/// Drop compiled engines no policy references any more.
fn prune_compiled(inner: &mut Inner) {
    let live: std::collections::HashSet<&String> =
        inner.policies.iter().map(|p| &p.content_hash).collect();
    let dead: Vec<String> = inner
        .compiled
        .keys()
        .filter(|h| !live.contains(h))
        .cloned()
        .collect();
    for h in dead {
        inner.compiled.remove(&h);
    }
}

/// Does `scope` cover `workspace`?
///
/// Empty is global, a trailing slash is a path prefix, anything else is exact.
/// Mirrors the prefix semantics already used for listing state.
fn scope_matches(scope: &str, workspace: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    if scope.ends_with('/') {
        return workspace.starts_with(scope);
    }
    scope == workspace
}

/// Policy names become filenames, so they may not contain path syntax.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 {
        return Err("policy name must be 1–128 characters".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err("policy name may only contain letters, digits, '-', '_' and '.'".to_string());
    }
    if name.starts_with('.') {
        return Err("policy name may not start with '.'".to_string());
    }
    Ok(())
}

fn hash_source(source: &str) -> String {
    crate::registry::hex_sha256(source.as_bytes())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Write via temp file + rename, matching the durability the state store already
/// commits to (`state::atomic_write`).
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid policy path")
    })?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{file_name}.{}.{seq}.tmp", std::process::id()));

    let result = std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN_DENY: &str = r#"
package terrarium.plan

deny contains msg if {
    some rc in input.plan.resource_changes
    "delete" in rc.change.actions
    msg := sprintf("refusing to destroy %s", [rc.address])
}
"#;

    const PLAN_WARN: &str = r#"
package terrarium.plan

warn contains msg if {
    some rc in input.plan.resource_changes
    "create" in rc.change.actions
    msg := sprintf("creating %s", [rc.address])
}
"#;

    const STATE_DENY: &str = r#"
package terrarium.state

deny contains msg if {
    input.state.serial > 100
    msg := "serial too high"
}
"#;

    fn plan_input() -> Value {
        Value::from_json_str(
            r#"{ "plan": { "resource_changes": [
                { "address": "aws_db_instance.main", "change": { "actions": ["delete"] } },
                { "address": "aws_s3_bucket.assets", "change": { "actions": ["create"] } }
            ] } }"#,
        )
        .unwrap()
    }

    fn tmp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "terrarium-policy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn tmp_store() -> (PolicyStore, PathBuf) {
        let base = tmp_dir();
        (
            PolicyStore::with_timeout(base.clone(), Duration::from_secs(5)),
            base,
        )
    }

    #[test]
    fn scope_matching() {
        assert!(scope_matches("", "infra/prod"));
        assert!(scope_matches("infra/prod", "infra/prod"));
        assert!(!scope_matches("infra/prod", "infra/staging"));
        assert!(scope_matches("infra/", "infra/prod"));
        assert!(scope_matches("infra/", "infra/prod/db"));
        assert!(!scope_matches("infra/", "apps/prod"));
        // A prefix scope must not match the bare parent name.
        assert!(!scope_matches("infra/", "infra"));
    }

    #[test]
    fn name_validation_rejects_path_syntax() {
        assert!(validate_name("no-public-s3").is_ok());
        assert!(validate_name("../../etc/passwd").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(".hidden").is_err());
    }

    #[test]
    fn deny_and_warn_are_attributed_to_their_policy() {
        let (store, base) = tmp_store();
        store.put("db-guard", PLAN_DENY, "", true, "alice").unwrap();
        store
            .put("creations", PLAN_WARN, "", true, "alice")
            .unwrap();

        let outcome = store.evaluate("infra/prod", Site::Plan, &plan_input());
        assert_eq!(outcome.evaluated, 2);
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);

        let deny: Vec<_> = outcome.denied().collect();
        assert_eq!(deny.len(), 1);
        assert_eq!(deny[0].policy, "db-guard");
        assert!(deny[0].message.contains("aws_db_instance.main"));

        let warn: Vec<_> = outcome
            .violations
            .iter()
            .filter(|v| v.severity == Severity::Warn)
            .collect();
        assert_eq!(warn.len(), 1);
        assert_eq!(warn[0].policy, "creations");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_policy_missing_warn_is_not_an_error() {
        // eval_rule treats an absent rule as an error; most policies define
        // deny without warn, so this must stay clean.
        let (store, base) = tmp_store();
        store.put("db-guard", PLAN_DENY, "", true, "alice").unwrap();
        let outcome = store.evaluate("infra/prod", Site::Plan, &plan_input());
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn state_policies_do_not_run_against_plans() {
        let (store, base) = tmp_store();
        store.put("serial", STATE_DENY, "", true, "alice").unwrap();

        let plan = store.evaluate("infra/prod", Site::Plan, &plan_input());
        assert_eq!(plan.evaluated, 0);

        let state_input = Value::from_json_str(r#"{ "state": { "serial": 500 } }"#).unwrap();
        let state = store.evaluate("infra/prod", Site::State, &state_input);
        assert_eq!(state.evaluated, 1);
        assert_eq!(state.violations.len(), 1);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn scope_limits_which_policies_run() {
        let (store, base) = tmp_store();
        store.put("global", PLAN_DENY, "", true, "alice").unwrap();
        store
            .put("prod-only", PLAN_DENY, "infra/prod", true, "alice")
            .unwrap();
        store
            .put("apps-only", PLAN_DENY, "apps/", true, "alice")
            .unwrap();

        assert_eq!(
            store
                .evaluate("infra/prod", Site::Plan, &plan_input())
                .evaluated,
            2
        );
        assert_eq!(
            store
                .evaluate("apps/web", Site::Plan, &plan_input())
                .evaluated,
            2
        );
        assert_eq!(
            store.evaluate("other", Site::Plan, &plan_input()).evaluated,
            1
        );

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn disabled_policies_do_not_run() {
        let (store, base) = tmp_store();
        store
            .put("db-guard", PLAN_DENY, "", false, "alice")
            .unwrap();
        assert_eq!(
            store
                .evaluate("infra/prod", Site::Plan, &plan_input())
                .evaluated,
            0
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn broken_rego_is_rejected_at_write_time() {
        let (store, base) = tmp_store();
        let err = store.put(
            "broken",
            "package terrarium.plan\ndeny contains {",
            "",
            true,
            "alice",
        );
        assert!(matches!(err, Err(PutError::Compile(_))));
        // Nothing was persisted.
        assert!(store.list().is_empty());
        assert!(!store.source_path("broken").exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn unknown_package_is_rejected() {
        // A typo'd package would store fine, never run, and silently protect
        // nothing — the worst possible failure mode for a policy engine.
        let (store, base) = tmp_store();
        let err = store.put(
            "typo",
            "package terrarium.plans\n\ndeny contains \"x\" if { true }",
            "",
            true,
            "alice",
        );
        assert!(matches!(err, Err(PutError::Compile(_))), "{err:?}");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn store_survives_reload_from_disk() {
        let (store, base) = tmp_store();
        store
            .put("db-guard", PLAN_DENY, "infra/", true, "alice")
            .unwrap();

        let reopened = PolicyStore::with_timeout(base.clone(), Duration::from_secs(5));
        let listed = reopened.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "db-guard");
        assert_eq!(listed[0].workspace, "infra/");
        assert_eq!(listed[0].origin, Origin::Api);
        assert_eq!(
            reopened
                .evaluate("infra/prod", Site::Plan, &plan_input())
                .evaluated,
            1
        );

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn hand_placed_files_are_adopted_as_file_origin() {
        let (_store, base) = tmp_store();
        std::fs::write(base.join("dropped-in.rego"), PLAN_DENY).unwrap();

        let store = PolicyStore::new(base.clone());
        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].origin, Origin::File);

        // File-owned policies cannot be overwritten or deleted through the API.
        assert!(matches!(
            store.put("dropped-in", PLAN_WARN, "", true, "alice"),
            Err(PutError::FileOwned)
        ));
        assert!(matches!(
            store.remove("dropped-in"),
            Err(PutError::FileOwned)
        ));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn removing_a_policy_stops_it_running() {
        let (store, base) = tmp_store();
        store.put("db-guard", PLAN_DENY, "", true, "alice").unwrap();
        assert!(store.remove("db-guard").unwrap());
        assert!(!store.remove("db-guard").unwrap());
        assert_eq!(
            store
                .evaluate("infra/prod", Site::Plan, &plan_input())
                .evaluated,
            0
        );
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn identical_source_under_two_names_shares_one_compiled_engine() {
        // The cache is keyed by content hash, so this is one entry, not two.
        let (store, base) = tmp_store();
        store.put("a", PLAN_DENY, "", true, "alice").unwrap();
        store.put("b", PLAN_DENY, "", true, "alice").unwrap();
        {
            let inner = store.inner.read().unwrap();
            assert_eq!(inner.policies.len(), 2);
            assert_eq!(inner.compiled.len(), 1);
        }

        // Rewriting one of them under new content adds an entry and retires
        // nothing, because the old hash is still referenced by the other.
        store.put("b", PLAN_WARN, "", true, "alice").unwrap();
        {
            let inner = store.inner.read().unwrap();
            assert_eq!(inner.compiled.len(), 2);
        }

        // Dropping the last reference to a hash retires its engine.
        store.remove("a").unwrap();
        {
            let inner = store.inner.read().unwrap();
            assert_eq!(inner.compiled.len(), 1);
        }

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_pathological_policy_is_stopped_by_the_timer() {
        // Rego forbids unbounded recursion, so runaway cost comes from nested
        // iteration. Without the execution timer this evaluation does not
        // finish in any useful amount of time.
        let pathological = r#"
package terrarium.plan

deny contains msg if {
    some i in numbers.range(1, 2000)
    some j in numbers.range(1, 2000)
    some k in numbers.range(1, 2000)
    i + j + k == 6000000
    msg := "never"
}
"#;
        let base = tmp_dir();
        let store = PolicyStore::with_timeout(base.clone(), Duration::from_millis(300));
        store.put("evil", pathological, "", true, "alice").unwrap();

        let started = std::time::Instant::now();
        let outcome = store.evaluate("infra/prod", Site::Plan, &plan_input());
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "evaluation ran {elapsed:?} — the timer did not bound it"
        );
        assert_eq!(outcome.errors.len(), 1, "expected a timeout error");
        assert!(outcome.violations.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }
}
