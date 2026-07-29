use std::io::{BufRead, IsTerminal, Write as _};
use colored::Colorize as _;
use serde_json::Value;
use crate::tofu::TofuBinary;

struct PlanResource {
    kind: String,   // resource type, e.g. "aws_instance"
    name: String,   // resource name, e.g. "web"
    action: String, // "create" | "update" | "delete" | "replace"
}

// ── Public entry points ──────────────────────────────────────────────────────

pub async fn run_plan_pretty(
    tofu: &TofuBinary,
    mut args: Vec<String>,
    policy_flag: Option<&str>,
) -> ! {
    let mode = parse_mode_flag(policy_flag);

    // Policy evaluation needs the plan as JSON, which requires a saved plan
    // file. If the caller did not ask for one, write to a temp file and clean
    // it up — a plan run should not leave artefacts behind.
    let user_supplied_out = args.iter().any(|a| a == "-out");
    let temp_out = (!user_supplied_out).then(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("terra-plan-{ts}.tfplan"))
    });

    if let Some(ref path) = temp_out {
        args.push("-out".to_string());
        args.push(path.to_string_lossy().to_string());
    }

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (status, has_changes) = plan_stream_inner(tofu, &refs);

    if status.success() && has_changes {
        let plan_path = temp_out.clone().map(|p| p.to_string_lossy().to_string()).or_else(|| {
            args.iter()
                .position(|a| a == "-out")
                .and_then(|i| args.get(i + 1))
                .cloned()
        });
        if let Some(path) = plan_path {
            // `enforcing: false` — a plan changes nothing, so violations are
            // reported here purely so they surface before apply time.
            crate::policy_client::gate(tofu, &path, mode, false).await;
        }
    }

    if let Some(path) = temp_out {
        let _ = std::fs::remove_file(path);
    }

    std::process::exit(status.code().unwrap_or(1));
}

/// Parse `--policy`, exiting on an unusable value rather than guessing.
fn parse_mode_flag(raw: Option<&str>) -> Option<crate::policy::Mode> {
    match raw {
        None => None,
        Some(v) => match crate::policy::Mode::parse(v.trim()) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("{} {e}", "error:".bold().red());
                std::process::exit(1);
            }
        },
    }
}

pub async fn run_apply_pretty(tofu: &TofuBinary, cmd: &crate::cli::ApplyCommand) -> ! {
    let mode = parse_mode_flag(cmd.policy.as_deref());

    // A pre-saved plan file skips the planning path entirely, so the gate has
    // to be applied here too — otherwise `terra apply saved.tfplan` would
    // quietly bypass every policy.
    if let Some(ref plan_file) = cmd.plan {
        if crate::policy_client::gate(tofu, plan_file, mode, true).await {
            std::process::exit(1);
        }
        let args = ["apply", "-json", "-no-color", "-auto-approve", plan_file.as_str()];
        let status = apply_stream(tofu, &args);
        std::process::exit(status.code().unwrap_or(1));
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let plan_file = std::env::temp_dir().join(format!("terra-plan-{ts}.tfplan"));
    let plan_file_str = plan_file.to_string_lossy().to_string();

    let plan_args = build_plan_args_from_apply(cmd, &plan_file_str);
    let plan_refs: Vec<&str> = plan_args.iter().map(|s| s.as_str()).collect();
    let (plan_status, has_changes) = plan_stream_inner(tofu, &plan_refs);

    if !plan_status.success() {
        let _ = std::fs::remove_file(&plan_file);
        std::process::exit(plan_status.code().unwrap_or(1));
    }

    if !has_changes {
        let _ = std::fs::remove_file(&plan_file);
        std::process::exit(0);
    }

    // Check before the confirmation prompt: being told a change is forbidden
    // after typing "yes" is a worse experience than being told before.
    if crate::policy_client::gate(tofu, &plan_file_str, mode, true).await {
        let _ = std::fs::remove_file(&plan_file);
        eprintln!("{}", "Apply blocked by policy.".bold().red());
        std::process::exit(1);
    }

    if !cmd.auto_approve {
        println!();
        print!("{}", "Do you want to perform these actions?\nEnter a value (yes to confirm): ".bold());
        std::io::stdout().flush().unwrap_or(());
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() || input.trim() != "yes" {
            println!("{}", "Apply cancelled.".yellow());
            let _ = std::fs::remove_file(&plan_file);
            std::process::exit(0);
        }
    }

    let apply_args = ["apply", "-json", "-no-color", "-auto-approve", &plan_file_str];
    let apply_status = apply_stream(tofu, &apply_args);
    let _ = std::fs::remove_file(&plan_file);
    std::process::exit(apply_status.code().unwrap_or(1));
}

// ── Plan streaming ────────────────────────────────────────────────────────────

fn plan_stream_inner(tofu: &TofuBinary, args: &[&str]) -> (std::process::ExitStatus, bool) {
    let mut child = tofu.spawn_piped(args).unwrap_or_else(|e| {
        eprintln!("{} {e}", "error:".bold().red());
        std::process::exit(1);
    });

    let stdout = child.stdout.take().unwrap();
    let reader = std::io::BufReader::new(stdout);
    let tty = std::io::stderr().is_terminal();

    let mut resources: Vec<PlanResource> = Vec::new();
    let mut drifted: Vec<PlanResource> = Vec::new();
    let mut summary: Option<(i64, i64, i64)> = None;
    let mut status_len: usize = 0; // current overwrite-line length (TTY only)

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };

        match v["type"].as_str().unwrap_or("") {
            // ── Refresh progress ───────────────────────────────────────────
            "refresh_start" => {
                if tty {
                    let addr = v["hook"]["resource"]["addr"].as_str().unwrap_or("...");
                    status_len = eprint_status(format!("  refreshing  {addr}"), status_len);
                }
            }
            "refresh_complete" => {} // next refresh_start overwrites

            // ── Plan results ───────────────────────────────────────────────
            "planned_change" => {
                clear_status(tty, &mut status_len);
                if let Some(r) = extract_res(&v["change"]) {
                    if r.action != "no-op" && r.action != "read" {
                        resources.push(r);
                    }
                }
            }
            "resource_drift" => {
                clear_status(tty, &mut status_len);
                if let Some(r) = extract_res(&v["change"]) {
                    drifted.push(r);
                }
            }
            "change_summary" => {
                clear_status(tty, &mut status_len);
                summary = Some((
                    v["changes"]["add"].as_i64().unwrap_or(0),
                    v["changes"]["change"].as_i64().unwrap_or(0),
                    v["changes"]["remove"].as_i64().unwrap_or(0),
                ));
            }
            "diagnostic" => {
                clear_status(tty, &mut status_len);
                print_diagnostic(&v);
            }
            _ => {
                if v["@level"].as_str() == Some("error") {
                    clear_status(tty, &mut status_len);
                    if let Some(msg) = v["@message"].as_str() {
                        eprintln!("{} {msg}", "error:".bold().red());
                    }
                }
            }
        }
    }

    clear_status(tty, &mut status_len);
    let status = child.wait().unwrap_or_else(|_| std::process::exit(1));

    if !drifted.is_empty() {
        eprintln!("{}", "Note: Objects have changed outside of OpenTofu".yellow().bold());
        print_grouped(&drifted);
        eprintln!();
    }

    let has_changes = match summary {
        Some((add, change, remove)) if add + change + remove == 0 => {
            println!("{}", "No changes. Infrastructure is up-to-date.".green());
            false
        }
        Some((add, change, remove)) => {
            print_plan_summary(add, change, remove);
            print_grouped(&resources);
            true
        }
        None => {
            if !resources.is_empty() { print_grouped(&resources); }
            !resources.is_empty()
        }
    };

    (status, has_changes)
}

// ── Apply streaming ───────────────────────────────────────────────────────────

fn apply_stream(tofu: &TofuBinary, args: &[&str]) -> std::process::ExitStatus {
    let mut child = tofu.spawn_piped(args).unwrap_or_else(|e| {
        eprintln!("{} {e}", "error:".bold().red());
        std::process::exit(1);
    });

    let stdout = child.stdout.take().unwrap();
    let reader = std::io::BufReader::new(stdout);
    let tty = std::io::stderr().is_terminal();
    let mut summary: Option<(i64, i64, i64)> = None;
    let mut status_len: usize = 0;

    println!("{}", "Applying...".bold());

    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };

        match v["type"].as_str().unwrap_or("") {
            "apply_start" => {
                if tty {
                    if let Some(r) = extract_hook(&v) {
                        let verb = action_verb_present(&r.action);
                        status_len = eprint_status(
                            format!("  {verb}  {}.{}", r.kind, r.name),
                            status_len,
                        );
                    }
                }
            }
            "apply_complete" => {
                clear_status(tty, &mut status_len);
                if let Some(r) = extract_hook(&v) {
                    let elapsed = v["hook"]["elapsed_seconds"].as_u64().unwrap_or(0);
                    print_apply_line(&r.kind, &r.name, &r.action, elapsed, false);
                }
            }
            "apply_errored" => {
                clear_status(tty, &mut status_len);
                if let Some(r) = extract_hook(&v) {
                    let elapsed = v["hook"]["elapsed_seconds"].as_u64().unwrap_or(0);
                    print_apply_line(&r.kind, &r.name, &r.action, elapsed, true);
                }
            }
            "change_summary" => {
                clear_status(tty, &mut status_len);
                summary = Some((
                    v["changes"]["add"].as_i64().unwrap_or(0),
                    v["changes"]["change"].as_i64().unwrap_or(0),
                    v["changes"]["remove"].as_i64().unwrap_or(0),
                ));
            }
            "diagnostic" => {
                clear_status(tty, &mut status_len);
                print_diagnostic(&v);
            }
            _ => {}
        }
    }

    clear_status(tty, &mut status_len);
    let status = child.wait().unwrap_or_else(|_| std::process::exit(1));

    println!();
    if let Some((add, change, remove)) = summary {
        print_apply_summary(add, change, remove, status.success());
    }

    status
}

// ── TTY status line helpers ───────────────────────────────────────────────────

/// Overwrite current stderr line with `msg`, returning the visible msg length.
fn eprint_status(msg: String, prev_len: usize) -> usize {
    let padded = format!("\r{:<width$}", msg, width = prev_len.max(msg.len()));
    eprint!("{}", padded.dimmed());
    let _ = std::io::stderr().flush();
    msg.len()
}

/// Clear the overwrite line if one is active.
fn clear_status(tty: bool, len: &mut usize) {
    if tty && *len > 0 {
        eprint!("\r{}\r", " ".repeat(*len));
        let _ = std::io::stderr().flush();
        *len = 0;
    }
}

// ── JSON extraction ───────────────────────────────────────────────────────────

/// Extract from a `change` object (planned_change / resource_drift).
fn extract_res(change: &Value) -> Option<PlanResource> {
    let res    = &change["resource"];
    // OpenTofu uses snake_case in its JSON log output
    let kind   = res["resource_type"].as_str().filter(|s| !s.is_empty())?;
    let name   = res["resource_name"].as_str().unwrap_or("");
    let module = res["module"].as_str().unwrap_or("");
    let action = change["action"].as_str().unwrap_or("");

    let full_kind = if module.is_empty() {
        kind.to_string()
    } else {
        format!("{module}.{kind}")
    };
    Some(PlanResource { kind: full_kind, name: name.to_string(), action: action.to_string() })
}

/// Extract from a `hook` object (apply_start / apply_complete / apply_errored).
fn extract_hook(v: &Value) -> Option<PlanResource> {
    let hook   = &v["hook"];
    let res    = &hook["resource"];
    let kind   = res["resource_type"].as_str().filter(|s| !s.is_empty())?;
    let name   = res["resource_name"].as_str().unwrap_or("");
    let module = res["module"].as_str().unwrap_or("");
    let action = hook["action"].as_str().unwrap_or("");

    let full_kind = if module.is_empty() {
        kind.to_string()
    } else {
        format!("{module}.{kind}")
    };
    Some(PlanResource { kind: full_kind, name: name.to_string(), action: action.to_string() })
}

fn action_verb_present(action: &str) -> &'static str {
    match action {
        "create"  => "creating",
        "update"  => "updating",
        "delete"  => "deleting",
        "replace" => "replacing",
        _         => "applying",
    }
}

// ── Display ───────────────────────────────────────────────────────────────────

fn print_grouped(resources: &[PlanResource]) {
    let mut groups: Vec<(&str, Vec<&PlanResource>)> = Vec::new();
    for r in resources {
        if let Some(g) = groups.iter_mut().find(|(k, _)| *k == r.kind.as_str()) {
            g.1.push(r);
        } else {
            groups.push((r.kind.as_str(), vec![r]));
        }
    }

    println!();
    for (i, (kind, members)) in groups.iter().enumerate() {
        if i > 0 { println!(); }
        println!("  {}", kind.bold());
        for r in members {
            match r.action.as_str() {
                "create"  => println!("    {} {}", "+".bold().green(),   r.name.green()),
                "update"  => println!("    {} {}", "~".bold().yellow(),  r.name.yellow()),
                "delete"  => println!("    {} {}", "-".bold().red(),     r.name.red()),
                "replace" => println!("    {} {}", "±".bold().magenta(), r.name.magenta()),
                _         => println!("    {} {}", "?".dimmed(),         r.name.dimmed()),
            }
        }
    }
}

fn print_apply_line(kind: &str, name: &str, action: &str, elapsed: u64, errored: bool) {
    let elapsed_str = if elapsed > 0 { format!(" ({elapsed}s)").dimmed().to_string() } else { String::new() };
    let label = format!("{kind}.{name}");
    if errored {
        println!("  {} {}  {}{}", "✗".bold().red(), label.red(), "failed".red(), elapsed_str);
        return;
    }
    match action {
        "create"  => println!("  {} {}  {}{}", "+".bold().green(),   label.green(),   "created".dimmed(),  elapsed_str),
        "update"  => println!("  {} {}  {}{}", "~".bold().yellow(),  label.yellow(),  "updated".dimmed(),  elapsed_str),
        "delete"  => println!("  {} {}  {}{}", "-".bold().red(),     label.red(),     "deleted".dimmed(),  elapsed_str),
        "replace" => println!("  {} {}  {}{}", "±".bold().magenta(), label.magenta(), "replaced".dimmed(), elapsed_str),
        _         => println!("  {} {}  {}{}", "·".dimmed(),         label,           "done".dimmed(),     elapsed_str),
    }
}

fn print_plan_summary(add: i64, change: i64, remove: i64) {
    let mut parts: Vec<String> = Vec::new();
    if add > 0    { parts.push(format!("{} to add",     add.to_string().green())); }
    if change > 0 { parts.push(format!("{} to change",  change.to_string().yellow())); }
    if remove > 0 { parts.push(format!("{} to destroy", remove.to_string().red())); }
    println!("{}: {}", "Plan".bold(), parts.join(", "));
}

fn print_apply_summary(add: i64, change: i64, remove: i64, success: bool) {
    if !success {
        println!("{}", "Apply failed!".bold().red());
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if add > 0    { parts.push(format!("{} added",     add.to_string().green())); }
    if change > 0 { parts.push(format!("{} changed",   change.to_string().yellow())); }
    if remove > 0 { parts.push(format!("{} destroyed", remove.to_string().red())); }
    let detail = if parts.is_empty() { "nothing to do".dimmed().to_string() } else { parts.join(", ") };
    println!("{} {}", "Apply complete!".bold().green(), detail);
}

fn print_diagnostic(v: &Value) {
    let severity = v["diagnostic"]["severity"].as_str().unwrap_or("error");
    let msg      = v["diagnostic"]["summary"].as_str().unwrap_or("");
    let detail   = v["diagnostic"]["detail"].as_str().unwrap_or("");
    if severity == "error" {
        eprintln!("{} {}", "error:".bold().red(), msg);
    } else {
        eprintln!("{} {}", "warning:".bold().yellow(), msg);
    }
    for line in detail.lines() {
        eprintln!("  {line}");
    }
}

// ── Arg builder ───────────────────────────────────────────────────────────────

fn build_plan_args_from_apply(cmd: &crate::cli::ApplyCommand, out: &str) -> Vec<String> {
    let mut args = vec![
        "plan".to_string(), "-json".to_string(), "-no-color".to_string(),
        "-out".to_string(), out.to_string(),
    ];
    if cmd.destroy      { args.push("-destroy".to_string()); }
    if cmd.refresh_only { args.push("-refresh-only".to_string()); }
    for r in &cmd.replace { args.extend(["-replace".to_string(), r.clone()]); }
    if cmd.no_input     { args.push("-input=false".to_string()); }
    if cmd.no_lock      { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    if let Some(p) = cmd.parallelism       { args.push(format!("-parallelism={p}")); }
    for v  in &cmd.var      { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    args
}
