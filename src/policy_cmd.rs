//! `terra policy …` — author locally, push to share.
//!
//! The authoring loop is deliberately server-free: write a rule in
//! `.terrarium/policies/`, check it with `terra policy test`, commit it, and
//! only then push it so the team and the server-side linter pick it up. Git is
//! the version history; the server is the distribution point.

use std::path::PathBuf;

use colored::Colorize as _;

use crate::cli;
use crate::client::TerrariumClient;
use crate::config;
use crate::policy::{Mode, Origin, Site};
use crate::policy_client::{cwd, discover_local};

const LOCAL_DIR: &str = ".terrarium/policies";

pub async fn handle(cmd: cli::PolicyCommand) {
    // `test` is the authoring loop and must work with no server configured at
    // all — that is the whole point of local-first.
    if let cli::PolicySubCommand::Test(args) = &cmd.subcommand {
        run_test(args);
        return;
    }

    let config = match config::load(cmd.config.map(PathBuf::from)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {e}", "configuration error:".bold().red());
            std::process::exit(1);
        }
    };
    let client = TerrariumClient::new(config.url, config.username, config.password);

    match cmd.subcommand {
        cli::PolicySubCommand::Test(_) => unreachable!("handled above"),
        cli::PolicySubCommand::List(_) => run_list(&client).await,
        cli::PolicySubCommand::Push(args) => run_push(&client, &args).await,
        cli::PolicySubCommand::Pull(args) => run_pull(&client, &args).await,
        cli::PolicySubCommand::Diff(args) => run_diff(&client, args.workspace.as_deref()).await,
        cli::PolicySubCommand::Rm(args) => match client.remove_policy(&args.name).await {
            Ok(()) => println!("{} {}", "Removed".green(), args.name),
            Err(e) => die(e),
        },
        cli::PolicySubCommand::Config(args) => run_config(&client, args.workspace.as_deref()).await,
    }
}

fn die(msg: String) -> ! {
    eprintln!("{} {msg}", "error:".bold().red());
    std::process::exit(1);
}

fn local_dir() -> PathBuf {
    cwd().join(LOCAL_DIR)
}

fn run_test(args: &cli::PolicyTest) {
    let local = discover_local(&cwd());
    if local.is_empty() {
        eprintln!("{}", format!("No policies found in {LOCAL_DIR}").dimmed());
        std::process::exit(1);
    }

    let raw = match std::fs::read_to_string(&args.input) {
        Ok(s) => s,
        Err(e) => die(format!("could not read {}: {e}", args.input)),
    };
    let doc: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => die(format!("{} is not valid JSON: {e}", args.input)),
    };

    let (site, key) = if args.state {
        (Site::State, "state")
    } else {
        (Site::Plan, "plan")
    };

    let input = serde_json::json!({
        "workspace": "",
        "user": crate::policy_client::policy_user(),
        key: doc,
    });
    let input = match regorus::Value::from_json_str(&input.to_string()) {
        Ok(v) => v,
        Err(e) => die(format!("could not build policy input: {e}")),
    };

    let sources: Vec<(String, String, Origin)> = local
        .into_iter()
        .map(|(n, s)| (n, s, Origin::Local))
        .collect();

    let outcome =
        crate::policy::evaluate_sources(&sources, site, &input, std::time::Duration::from_secs(5));

    for v in &outcome.violations {
        let mark = match v.severity {
            crate::policy::Severity::Deny => "✗".bold().red(),
            crate::policy::Severity::Warn => "!".bold().yellow(),
        };
        println!("  {mark} {}  {}", v.policy, v.message);
    }
    for (policy, err) in &outcome.errors {
        println!("  {} {policy}  {err}", "?".bold().yellow());
    }

    let denied = outcome.denied().count();
    if outcome.violations.is_empty() && outcome.errors.is_empty() {
        println!(
            "{} {}",
            "Passed".green(),
            format!("({} policies)", outcome.evaluated).dimmed()
        );
    } else {
        println!(
            "{} {denied} denied, {} warning(s)",
            "Result:".bold(),
            outcome.violations.len() - denied
        );
    }

    // Non-zero on deny so the authoring loop composes with scripts and CI.
    std::process::exit(if denied > 0 { 1 } else { 0 });
}

async fn run_list(client: &TerrariumClient) {
    match client.list_policies().await {
        Ok(policies) if policies.is_empty() => println!("{}", "No policies on the server".dimmed()),
        Ok(policies) => {
            println!("{}", "Policies:".bold());
            for p in policies {
                let scope = if p.workspace.is_empty() {
                    "global".to_string()
                } else {
                    p.workspace.clone()
                };
                let sites: Vec<&str> = p
                    .sites
                    .iter()
                    .map(|s| match s {
                        Site::State => "state",
                        Site::Plan => "plan",
                    })
                    .collect();
                let origin = match p.origin {
                    Origin::File => " [file]",
                    Origin::Local => " [local]",
                    Origin::Api => "",
                };
                let state = if p.enabled { "" } else { " (disabled)" };
                println!(
                    "  {}{}  {}  {}{}",
                    p.name.bold(),
                    origin.dimmed(),
                    scope.cyan(),
                    sites.join(",").dimmed(),
                    state.dimmed()
                );
            }
        }
        Err(e) => die(e),
    }
}

async fn run_push(client: &TerrariumClient, args: &cli::PolicyPush) {
    let local = discover_local(&cwd());
    if local.is_empty() {
        eprintln!("{}", format!("No policies found in {LOCAL_DIR}").dimmed());
        return;
    }

    let workspace = args.workspace.clone().unwrap_or_default();
    let remote = client.list_policies().await.unwrap_or_default();

    for (name, source) in &local {
        let existing = remote.iter().find(|p| &p.name == name);
        let unchanged = existing.is_some_and(|p| {
            p.content_hash == crate::policy::hash_source_pub(source) && p.workspace == workspace
        });

        if unchanged {
            println!("  {} {name}", "=".dimmed());
            continue;
        }

        let verb = if existing.is_some() {
            "update"
        } else {
            "create"
        };
        if args.dry_run {
            println!("  {} {name} ({verb})", "~".yellow());
            continue;
        }

        match client.put_policy(name, source, &workspace, true).await {
            Ok(()) => println!("  {} {name} ({verb}d)", "+".green()),
            Err(e) => {
                eprintln!("  {} {name}: {e}", "✗".red());
                std::process::exit(1);
            }
        }
    }

    if args.dry_run {
        println!("{}", "Dry run — nothing was written.".dimmed());
    }
}

async fn run_pull(client: &TerrariumClient, args: &cli::PolicyPull) {
    let workspace = args.workspace.clone().unwrap_or_default();
    let bundle = match client.policy_bundle(&workspace).await {
        Ok(b) => b,
        Err(e) => die(e.to_string()),
    };

    if bundle.policies.is_empty() {
        println!("{}", "Server has no policies for this workspace".dimmed());
        return;
    }

    let dir = local_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        die(format!("could not create {}: {e}", dir.display()));
    }

    for p in &bundle.policies {
        let path = dir.join(format!("{}.rego", p.policy.name));
        match std::fs::write(&path, &p.source) {
            Ok(()) => println!("  {} {}", "+".green(), path.display()),
            Err(e) => die(format!("could not write {}: {e}", path.display())),
        }
    }
    println!(
        "{}",
        "Pulled into the repo — commit them so the team shares one source of truth.".dimmed()
    );
}

async fn run_diff(client: &TerrariumClient, workspace: Option<&str>) {
    let workspace = workspace.unwrap_or("");
    let local = discover_local(&cwd());
    let bundle = match client.policy_bundle(workspace).await {
        Ok(b) => b,
        Err(e) => die(e.to_string()),
    };

    let mut clean = true;

    for (name, source) in &local {
        match bundle.policies.iter().find(|p| &p.policy.name == name) {
            Some(remote) if remote.source.trim() != source.trim() => {
                println!("  {} {name}  {}", "~".yellow(), "differs".yellow());
                clean = false;
            }
            Some(_) => {}
            None => {
                println!("  {} {name}  {}", "+".green(), "local only".dimmed());
                clean = false;
            }
        }
    }

    // Server-only policies are listed for orientation, not flagged: global
    // rules legitimately live outside any one repository.
    for p in &bundle.policies {
        if !local.iter().any(|(n, _)| n == &p.policy.name) {
            println!(
                "  {} {}  {}",
                "·".dimmed(),
                p.policy.name,
                "server only".dimmed()
            );
        }
    }

    if clean {
        println!("{}", "In sync with the server.".green());
    }
}

async fn run_config(client: &TerrariumClient, workspace: Option<&str>) {
    match workspace {
        Some(ws) => match client.policy_bundle(ws).await {
            Ok(bundle) => {
                let c = &bundle.config;
                println!("{} {ws}", "Workspace:".bold());
                println!(
                    "  mode              {}  {}",
                    c.mode.as_str().cyan(),
                    match &c.from_scope {
                        Some(s) if s.is_empty() => "(from global config)".dimmed(),
                        Some(s) => format!("(from scope {s:?})").dimmed(),
                        None => "(built-in default)".dimmed(),
                    }
                );
                println!("  lint on push      {}", c.lint);
                println!("  max state bytes   {}", c.max_state_bytes);
                println!(
                    "  policies applied  {}",
                    bundle.policies.len().to_string().bold()
                );
            }
            Err(e) => die(e.to_string()),
        },
        None => match client.policy_config().await {
            Ok(entries) if entries.is_empty() => println!(
                "{}",
                format!(
                    "No scoped configuration — defaults apply (mode: {}, lint on).",
                    Mode::default().as_str()
                )
                .dimmed()
            ),
            Ok(entries) => {
                println!("{}", "Policy configuration:".bold());
                for e in entries {
                    let scope = if e.scope.is_empty() {
                        "global".to_string()
                    } else {
                        e.scope.clone()
                    };
                    println!(
                        "  {}  mode={} lint={}",
                        scope.cyan(),
                        e.mode.as_str(),
                        e.lint
                    );
                }
            }
            Err(e) => die(e),
        },
    }
}
