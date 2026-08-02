//! Client-side policy checking for `terra plan` and `terra apply`.
//!
//! Policies come from two places and are unioned: the server bundle for the
//! workspace, and any `.rego` files the repository carries in
//! `.terrarium/policies/`. Repository policies can therefore only *add*
//! restrictions — the bundle is fetched independently and a repo cannot edit it.
//!
//! Weakening remains possible with `--policy=off`, deliberately: that puts the
//! decision on the command line, visible in shell history and CI logs, rather
//! than hidden in a file inside the repo. And it hides nothing either way — the
//! resulting state is still linted when it reaches the server.

use std::path::{Path, PathBuf};
use std::time::Duration;

use colored::Colorize as _;
use serde::Deserialize;

use crate::client::{BundleError, TerrariumClient};
use crate::policy::{Bundle, Mode, Origin, Outcome, Severity, Site};

/// Directory a repository keeps its own policies in, relative to the repo root.
const LOCAL_DIR: &str = ".terrarium/policies";
/// Optional repository-level settings file.
const LOCAL_CONFIG: &str = ".terrarium/policy.json";
/// How far up the tree to look before giving up.
const MAX_WALK_DEPTH: usize = 32;

/// Repository-level settings. May raise strictness, never lower it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LocalConfig {
    #[serde(default)]
    pub mode: Option<Mode>,
}

/// Everything the client needs to decide and explain a verdict.
pub struct Prepared {
    pub sources: Vec<(String, String, Origin)>,
    /// Identity associated with this client-side plan evaluation.
    pub user: String,
    pub mode: Mode,
    /// How the mode was arrived at, for `--verbose`-free explanation.
    pub mode_reason: String,
    pub server_count: usize,
    pub local_count: usize,
    /// Warnings about the fetch or about drift, printed before any verdict.
    pub notices: Vec<Notice>,
}

pub enum Notice {
    /// Dim, unobtrusive — expected states that should not nag.
    Info(String),
    /// Yellow — anomalies worth noticing.
    Warn(String),
}

impl Notice {
    pub fn print(&self) {
        match self {
            Self::Info(m) => eprintln!("{}", m.dimmed()),
            Self::Warn(m) => eprintln!("{} {m}", "warning:".bold().yellow()),
        }
    }
}

/// Find `.terrarium/policies/*.rego` by walking up from `start`.
///
/// Terraform is usually run from a module directory well below the repository
/// root, so looking only in the working directory would miss the common layout.
pub fn discover_local(start: &Path) -> Vec<(String, String)> {
    let mut dir = Some(start);
    let mut depth = 0;

    while let Some(current) = dir {
        if depth > MAX_WALK_DEPTH {
            break;
        }
        let candidate = current.join(LOCAL_DIR);
        if candidate.is_dir() {
            return read_rego_dir(&candidate);
        }
        // Stop at a repository boundary: past it we are in somebody else's
        // tree, and silently applying their policies would be surprising.
        if current.join(".git").exists() {
            break;
        }
        dir = current.parent();
        depth += 1;
    }

    Vec::new()
}

/// Locate the repository-level policy settings, if any.
pub fn discover_local_config(start: &Path) -> LocalConfig {
    let mut dir = Some(start);
    let mut depth = 0;

    while let Some(current) = dir {
        if depth > MAX_WALK_DEPTH {
            break;
        }
        let candidate = current.join(LOCAL_CONFIG);
        if candidate.is_file() {
            return std::fs::read_to_string(&candidate)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        }
        if current.join(".git").exists() {
            break;
        }
        dir = current.parent();
        depth += 1;
    }

    LocalConfig::default()
}

fn read_rego_dir(dir: &Path) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(dir)
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
                .collect()
        })
        .unwrap_or_default();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Compare repository policies against the server bundle.
///
/// Only same-name-different-content is unambiguous drift. "The server has rules
/// my repo doesn't" is normal — global rules need not live in every repo — and
/// warning about it would train people to ignore the warning.
fn drift_notices(local: &[(String, String)], bundle: &Bundle) -> Vec<Notice> {
    let mut notices = Vec::new();
    let mut diverged: Vec<&str> = Vec::new();
    let mut unpushed: Vec<&str> = Vec::new();

    for (name, source) in local {
        match bundle.policies.iter().find(|p| &p.policy.name == name) {
            Some(remote) if remote.source.trim() != source.trim() => diverged.push(name),
            Some(_) => {}
            None => unpushed.push(name),
        }
    }

    if !diverged.is_empty() {
        notices.push(Notice::Warn(format!(
            "{} differ(s) between this repo and the server: {}. \
             Both versions were evaluated. Run `terra policy push` to reconcile.",
            if diverged.len() == 1 {
                "policy"
            } else {
                "policies"
            },
            diverged.join(", ")
        )));
    }
    if !unpushed.is_empty() {
        notices.push(Notice::Info(format!(
            "{} local policy/policies not on the server: {} (terra policy push)",
            unpushed.len(),
            unpushed.join(", ")
        )));
    }

    notices
}

/// Assemble the policy set and the mode in force.
///
/// `server` is `None` when no Terrarium server is configured at all — `terra`
/// used purely as a `tofu` wrapper, which is the documented headline use and
/// must not be made to fail.
pub async fn prepare(
    server: Option<(&TerrariumClient, &str, &str)>,
    cwd: &Path,
    flag_mode: Option<Mode>,
) -> Result<Prepared, String> {
    let local = discover_local(cwd);
    let local_config = discover_local_config(cwd);
    let mut notices = Vec::new();

    let bundle = match server {
        Some((client, workspace, _user)) => match client.policy_bundle(workspace).await {
            Ok(b) => Some(b),
            Err(BundleError::NotSupported) => {
                notices.push(Notice::Warn(
                    "server does not support policies (older version) — \
                     checking repository policies only"
                        .to_string(),
                ));
                None
            }
            Err(e @ (BundleError::Unreachable(_) | BundleError::Unauthorized)) => {
                // The plan just succeeded, which means the server answered
                // seconds ago. Going quiet now is an anomaly, not a normal
                // offline state, so stop rather than quietly skipping checks.
                return Err(format!(
                    "could not fetch policies: {e}\n       \
                     re-run with --policy=off to apply without checking"
                ));
            }
            Err(e) => return Err(format!("could not fetch policies: {e}")),
        },
        None => None,
    };

    if let Some(ref b) = bundle {
        notices.extend(drift_notices(&local, b));
    }

    let mut sources: Vec<(String, String, Origin)> = Vec::new();
    if let Some(ref b) = bundle {
        for p in &b.policies {
            sources.push((p.policy.name.clone(), p.source.clone(), p.policy.origin));
        }
    }
    let server_count = sources.len();
    for (name, source) in &local {
        sources.push((name.clone(), source.clone(), Origin::Local));
    }

    // Precedence: flag > env > repo config (raise only) > server > default.
    let server_mode = bundle.as_ref().map(|b| b.config.mode);
    let (mode, mode_reason) = resolve_mode(flag_mode, local_config.mode, server_mode);

    Ok(Prepared {
        sources,
        // A configured Terrarium identity is authoritative. Local-only checks
        // use the same environment fallback as `terra policy test`.
        user: server
            .map(|(_, _, user)| user.to_string())
            .unwrap_or_else(policy_user),
        mode,
        mode_reason,
        server_count,
        local_count: local.len(),
        notices,
    })
}

/// Resolve the effective mode and say where it came from.
fn resolve_mode(flag: Option<Mode>, local: Option<Mode>, server: Option<Mode>) -> (Mode, String) {
    if let Some(m) = flag {
        return (m, "--policy flag".to_string());
    }
    if let Ok(raw) = std::env::var("TERRARIUM_POLICY_MODE")
        && let Ok(m) = Mode::parse(raw.trim())
    {
        return (m, "TERRARIUM_POLICY_MODE".to_string());
    }

    let base = server.unwrap_or_default();
    if let Some(l) = local {
        // A repository may tighten what the server asked for, never loosen it:
        // loosening must be visible on the command line, not buried in a file.
        if l.at_least_as_strict_as(base) {
            return (
                l,
                format!(".terrarium/policy.json (raised from {})", base.as_str()),
            );
        }
        return (
            base,
            format!(
                "server config ({} in .terrarium/policy.json ignored — a repo may not weaken it)",
                l.as_str()
            ),
        );
    }

    match server {
        Some(_) => (base, "server config".to_string()),
        None => (base, "default".to_string()),
    }
}

/// Run the policy check for a saved plan file.
///
/// Returns `true` when the caller must not proceed. `enforcing` is false for
/// `terra plan`, which reports but never blocks — a plan changes nothing.
///
/// Every failure mode here is soft *except* an unreachable-but-configured
/// server: a plan only succeeds if the backend answered, so a server going
/// quiet immediately afterwards is an anomaly, not routine offline work.
pub async fn gate(
    tofu: &crate::tofu::TofuBinary,
    plan_file: &str,
    flag_mode: Option<Mode>,
    enforcing: bool,
) -> bool {
    // No server configured at all: `terra` as a plain `tofu` wrapper, the
    // documented headline use. Repository policies still apply.
    let config = crate::config::load_quiet();

    let dir = cwd();
    let client_and_workspace = config.as_ref().map(|c| {
        let workspace = detect_workspace(&dir, &c.url).unwrap_or_default();
        (
            TerrariumClient::new(c.url.clone(), c.username.clone(), c.password.clone()),
            workspace,
            c.username.clone(),
        )
    });

    let has_local = !discover_local(&dir).is_empty();
    if client_and_workspace.is_none() && !has_local {
        return false; // nothing to check, and nothing to say about it
    }

    let prepared = match prepare(
        client_and_workspace
            .as_ref()
            .map(|(c, w, user)| (c, w.as_str(), user.as_str())),
        &dir,
        flag_mode,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {e}", "error:".bold().red());
            return enforcing;
        }
    };

    if prepared.mode == Mode::Off {
        eprintln!(
            "{}",
            format!("Policy check skipped ({}).", prepared.mode_reason).dimmed()
        );
        return false;
    }

    let plan_json = match tofu.run_json(&["show", "-json", plan_file]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "{} could not read the plan for policy checking: {e}",
                "warning:".bold().yellow()
            );
            return false;
        }
    };

    let workspace = client_and_workspace
        .as_ref()
        .map(|(_, w, _)| w.as_str())
        .unwrap_or("");

    let blocked = check_plan(&prepared, &plan_json, workspace);
    blocked && enforcing
}

/// Evaluate a plan document and print the result.
///
/// Returns `true` when the caller should stop. `terra plan` ignores the return
/// value — a plan changes nothing, so there is nothing to block.
pub fn check_plan(prepared: &Prepared, plan_json: &serde_json::Value, workspace: &str) -> bool {
    for notice in &prepared.notices {
        notice.print();
    }

    if prepared.mode == Mode::Off {
        return false;
    }
    if prepared.sources.is_empty() {
        return false;
    }

    let input = serde_json::json!({
        "workspace": workspace,
        "user": prepared.user,
        "plan": plan_json,
    });
    let input = match regorus::Value::from_json_str(&input.to_string()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "{} could not build policy input: {e}",
                "warning:".bold().yellow()
            );
            return false;
        }
    };

    let outcome = crate::policy::evaluate_sources(
        &prepared.sources,
        Site::Plan,
        &input,
        Duration::from_secs(5),
    );

    report(prepared, &outcome);

    let denied = outcome.denied().count();
    prepared.mode == Mode::Enforce && denied > 0
}

/// Print violations, then a one-line summary naming what actually ran.
///
/// The summary is not decoration: with two origins in play, "no violations"
/// must never be mistaken for "the server's rules passed".
fn report(prepared: &Prepared, outcome: &Outcome) {
    for v in &outcome.violations {
        let origin = match v.origin {
            Origin::Local => " (local)",
            _ => "",
        };
        match v.severity {
            Severity::Deny => eprintln!(
                "  {} {}{}  {}",
                "✗".bold().red(),
                v.policy.red(),
                origin.dimmed(),
                v.message
            ),
            Severity::Warn => eprintln!(
                "  {} {}{}  {}",
                "!".bold().yellow(),
                v.policy.yellow(),
                origin.dimmed(),
                v.message
            ),
        }
    }

    for (policy, err) in &outcome.errors {
        eprintln!("  {} {policy}  {err}", "?".bold().yellow());
    }

    let scope = if prepared.local_count > 0 && prepared.server_count > 0 {
        format!(
            "{} server, {} local",
            prepared.server_count, prepared.local_count
        )
    } else if prepared.local_count > 0 {
        format!(
            "{} local only — no server policies applied",
            prepared.local_count
        )
    } else {
        format!("{} server", prepared.server_count)
    };

    let denied = outcome.denied().count();
    let warned = outcome.violations.len() - denied;

    if outcome.violations.is_empty() && outcome.errors.is_empty() {
        eprintln!(
            "{} {}",
            "Policy check passed".green(),
            format!("({scope})").dimmed()
        );
    } else {
        eprintln!(
            "{} {denied} denied, {warned} warning(s) {}",
            "Policy check:".bold(),
            format!("({scope}, mode: {})", prepared.mode.as_str()).dimmed()
        );
    }
}

/// Identity for a local-only policy evaluation when no Terrarium config exists.
pub fn policy_user() -> String {
    std::env::var("TERRARIUM_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

/// Working directory, falling back to `.` so a missing cwd cannot abort a run.
pub fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Work out which Terrarium workspace this directory is bound to.
///
/// OpenTofu caches the resolved backend configuration in
/// `.terraform/terraform.tfstate` after `init`, and for the HTTP backend that
/// includes the state address — the only place the workspace name is written
/// down locally. Returns `None` for a local backend or a non-Terrarium address,
/// in which case only globally-scoped policies can apply.
pub fn detect_workspace(dir: &Path, base_url: &str) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join(".terraform").join("terraform.tfstate")).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;

    let backend = doc.get("backend")?;
    if backend.get("type")?.as_str()? != "http" {
        return None;
    }
    let address = backend.get("config")?.get("address")?.as_str()?;

    let base = base_url.trim_end_matches('/');
    let rest = address.strip_prefix(base)?;
    let name = rest.trim_start_matches('/').strip_prefix("state/")?;

    // Drop any query string (`?ID=…`) the backend may carry.
    let name = name.split('?').next().unwrap_or(name).trim_matches('/');
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "terrarium-local-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join(LOCAL_DIR)).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        base
    }

    #[test]
    fn discovery_walks_up_from_a_nested_module_directory() {
        let repo = tmp_repo();
        std::fs::write(
            repo.join(LOCAL_DIR).join("guard.rego"),
            "package terrarium.plan",
        )
        .unwrap();

        let nested = repo.join("envs").join("prod");
        std::fs::create_dir_all(&nested).unwrap();

        let found = discover_local(&nested);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "guard");

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn discovery_stops_at_a_repository_boundary() {
        // A parent repo's policies must not leak into a nested checkout.
        let outer = tmp_repo();
        std::fs::write(
            outer.join(LOCAL_DIR).join("outer.rego"),
            "package terrarium.plan",
        )
        .unwrap();

        let inner = outer.join("vendor").join("other");
        std::fs::create_dir_all(inner.join(".git")).unwrap();

        assert!(discover_local(&inner).is_empty());

        let _ = std::fs::remove_dir_all(outer);
    }

    #[test]
    fn a_repo_may_raise_strictness_but_not_lower_it() {
        // Raising is honoured.
        let (mode, _) = resolve_mode(None, Some(Mode::Enforce), Some(Mode::Warn));
        assert_eq!(mode, Mode::Enforce);

        // Lowering is ignored, and the server's mode stands.
        let (mode, why) = resolve_mode(None, Some(Mode::Off), Some(Mode::Enforce));
        assert_eq!(mode, Mode::Enforce);
        assert!(why.contains("may not weaken"));

        let (mode, _) = resolve_mode(None, Some(Mode::Warn), Some(Mode::Enforce));
        assert_eq!(mode, Mode::Enforce);
    }

    #[test]
    fn the_flag_beats_everything() {
        let (mode, why) = resolve_mode(Some(Mode::Off), Some(Mode::Enforce), Some(Mode::Enforce));
        assert_eq!(mode, Mode::Off);
        assert!(why.contains("flag"));
    }

    #[test]
    fn default_mode_without_a_server_is_enforce() {
        let (mode, _) = resolve_mode(None, None, None);
        assert_eq!(mode, Mode::Enforce);
    }

    #[test]
    fn drift_warns_only_on_diverging_content() {
        use crate::policy::{BundledPolicy, EffectiveConfig, Policy};

        let remote = |name: &str, source: &str| BundledPolicy {
            policy: Policy {
                name: name.to_string(),
                workspace: String::new(),
                enabled: true,
                origin: Origin::Api,
                sites: vec![Site::Plan],
                content_hash: String::new(),
                updated: String::new(),
                updated_by: String::new(),
            },
            source: source.to_string(),
        };

        let bundle = Bundle {
            workspace: "w".to_string(),
            policies: vec![
                remote("same", "A"),
                remote("changed", "A"),
                remote("server-only", "A"),
            ],
            config: EffectiveConfig::default(),
        };

        let local = vec![
            ("same".to_string(), "A".to_string()),
            ("changed".to_string(), "B".to_string()),
            ("new".to_string(), "C".to_string()),
        ];

        let notices = drift_notices(&local, &bundle);

        // One warning for the diverged policy, one info for the unpushed one,
        // and nothing at all for identical or server-only policies.
        let warns: Vec<&Notice> = notices
            .iter()
            .filter(|n| matches!(n, Notice::Warn(_)))
            .collect();
        assert_eq!(warns.len(), 1);
        match warns[0] {
            Notice::Warn(m) => {
                assert!(m.contains("changed"));
                assert!(!m.contains("server-only"));
                assert!(!m.contains("same"));
            }
            _ => unreachable!(),
        }

        let infos: Vec<&Notice> = notices
            .iter()
            .filter(|n| matches!(n, Notice::Info(_)))
            .collect();
        assert_eq!(infos.len(), 1);
        match infos[0] {
            Notice::Info(m) => assert!(m.contains("new")),
            _ => unreachable!(),
        }
    }
}
