use std::io::Write as _;

use authur::Roles;
use axum::{
    Router,
    middleware,
    routing::{get, post, put},
};
use colored::Colorize as _;

use crate::{lock::LockContainer, state::StateContainer, webhook::WebhookStore};
use crate::tofu::TofuBinary;
use crate::terranix::TerranixBinary;

mod cli;
mod client;
mod config;
pub mod lock;
pub mod registry;
pub mod state;
pub mod tfstate;
pub mod user;
pub mod webhook;
mod tofu;
mod terranix;
mod plan_json;
mod statediff;
mod ui;
mod auth;
mod observability;

#[derive(Clone)]
pub struct AppState {
    state: state::StateContainer,
    locks: lock::LockContainer,
    users: authur::UserDB<authur::vfs::PhysicalFS>,
    webhooks: webhook::WebhookStore,
    #[allow(dead_code)]
    tofu: Option<TofuBinary>,
    pub registry: registry::RegistryStore,
    pub mirror_status: registry::MirrorStatusRef,
}

impl axum::extract::FromRef<AppState> for authur::UserDB<authur::vfs::PhysicalFS> {
    fn from_ref(input: &AppState) -> Self {
        input.users.clone()
    }
}

/// Returns the data directory, honoring TERRARIUM_DATA env var.
fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("TERRARIUM_DATA").unwrap_or_else(|_| ".".to_string()),
    )
}

// ── Helper: convert tofu command structs to argument vectors ────────────

fn init_args(cmd: &cli::InitCommand) -> Vec<String> {
    let mut args = vec!["init".to_string()];
    if let Some(ref m) = cmd.from_module { args.extend(["-from-module".to_string(), m.clone()]); }
    if cmd.reconfigure { args.push("-reconfigure".to_string()); }
    if cmd.migrate_state { args.push("-migrate-state".to_string()); }
    for bc in &cmd.backend_config { args.extend(["-backend-config".to_string(), bc.clone()]); }
    if cmd.no_input { args.push("-input=false".to_string()); }
    if cmd.no_lock { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    if cmd.upgrade { args.push("-upgrade".to_string()); }
    if let Some(ref pd) = cmd.plugin_dir { args.extend(["-plugin-dir".to_string(), pd.clone()]); }
    if let Some(ref lf) = cmd.lockfile { args.extend(["-lockfile".to_string(), lf.clone()]); }
    if cmd.json { args.push("-json".to_string()); }
    for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    args
}

fn validate_args(cmd: &cli::ValidateCommand) -> Vec<String> {
    let mut args = vec!["validate".to_string()];
    if cmd.json { args.push("-json".to_string()); }
    args
}

fn plan_args(cmd: &cli::PlanCommand) -> Vec<String> {
    let mut args = vec!["plan".to_string()];
    if let Some(ref o) = cmd.out { args.extend(["-out".to_string(), o.clone()]); }
    if cmd.destroy { args.push("-destroy".to_string()); }
    if cmd.refresh_only { args.push("-refresh-only".to_string()); }
    for r in &cmd.replace { args.extend(["-replace".to_string(), r.clone()]); }
    for t in &cmd.target { args.extend(["-target".to_string(), t.clone()]); }
    if cmd.no_input { args.push("-input=false".to_string()); }
    if cmd.no_lock { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    if let Some(p) = cmd.parallelism { args.push(format!("-parallelism={p}")); }
    for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    if cmd.json { args.push("-json".to_string()); }
    if cmd.no_color { args.push("-no-color".to_string()); }
    if cmd.no_refresh { args.push("-refresh=false".to_string()); }
    args
}

fn apply_args(cmd: &cli::ApplyCommand) -> Vec<String> {
    let mut args = vec!["apply".to_string()];
    if cmd.auto_approve { args.push("-auto-approve".to_string()); }
    if let Some(ref p) = cmd.plan { args.push(p.clone()); }
    if cmd.no_input { args.push("-input=false".to_string()); }
    if cmd.no_lock { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    if let Some(p) = cmd.parallelism { args.push(format!("-parallelism={p}")); }
    for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    if cmd.json { args.push("-json".to_string()); }
    if cmd.no_color { args.push("-no-color".to_string()); }
    for r in &cmd.replace { args.extend(["-replace".to_string(), r.clone()]); }
    if cmd.destroy { args.push("-destroy".to_string()); }
    if cmd.refresh_only { args.push("-refresh-only".to_string()); }
    args
}

fn destroy_args(cmd: &cli::DestroyCommand) -> Vec<String> {
    let mut args = vec!["destroy".to_string()];
    if cmd.auto_approve { args.push("-auto-approve".to_string()); }
    for t in &cmd.target { args.extend(["-target".to_string(), t.clone()]); }
    if cmd.no_input { args.push("-input=false".to_string()); }
    if cmd.no_lock { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    if let Some(p) = cmd.parallelism { args.push(format!("-parallelism={p}")); }
    for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    if cmd.no_color { args.push("-no-color".to_string()); }
    args
}

fn console_args(_cmd: &cli::ConsoleCommand) -> Vec<String> {
    vec!["console".to_string()]
}

fn fmt_args(cmd: &cli::FmtCommand) -> Vec<String> {
    let mut args = vec!["fmt".to_string()];
    if cmd.check { args.push("-check".to_string()); }
    if cmd.recursive { args.push("-recursive".to_string()); }
    if cmd.diff { args.push("-diff".to_string()); }
    if cmd.stdio { args.push("-stdio".to_string()); }
    if cmd.list { args.push("-list".to_string()); }
    if cmd.no_color { args.push("-no-color".to_string()); }
    for p in &cmd.paths { args.push(p.clone()); }
    args
}

fn force_unlock_args(cmd: &cli::ForceUnlockCommand) -> Vec<String> {
    let mut args = vec!["force-unlock".to_string()];
    if cmd.force { args.push("-force".to_string()); }
    args.push(cmd.lock_id.clone());
    args
}

fn get_args(cmd: &cli::GetCommand) -> Vec<String> {
    let mut args = vec!["get".to_string()];
    if cmd.update { args.push("-update".to_string()); }
    args
}

fn graph_args(cmd: &cli::GraphCommand) -> Vec<String> {
    let mut args = vec!["graph".to_string()];
    if let Some(ref t) = cmd.type_ { args.extend(["-type".to_string(), t.clone()]); }
    args
}

fn import_args(cmd: &cli::ImportCommand) -> Vec<String> {
    let mut args = vec!["import".to_string()];
    args.push(cmd.address.clone());
    args.push(cmd.id.clone());
    if cmd.no_input { args.push("-input=false".to_string()); }
    if cmd.no_lock { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    args
}

fn login_args(cmd: &cli::LoginCommand) -> Vec<String> {
    let mut args = vec!["login".to_string()];
    if let Some(ref h) = cmd.hostname { args.push(h.clone()); }
    args
}

fn logout_args(cmd: &cli::LogoutCommand) -> Vec<String> {
    let mut args = vec!["logout".to_string()];
    if let Some(ref h) = cmd.hostname { args.push(h.clone()); }
    args
}

fn output_args(cmd: &cli::OutputCommand) -> Vec<String> {
    let mut args = vec!["output".to_string()];
    if cmd.json { args.push("-json".to_string()); }
    if cmd.raw { args.push("-raw".to_string()); }
    if let Some(ref n) = cmd.name { args.push(n.clone()); }
    args
}

fn providers_args(cmd: &cli::ProvidersCommand) -> Vec<String> {
    let mut args = vec!["providers".to_string()];
    if cmd.mirror { args.push("-mirror".to_string()); }
    if cmd.json { args.push("-json".to_string()); }
    args
}

fn refresh_args(cmd: &cli::RefreshCommand) -> Vec<String> {
    let mut args = vec!["refresh".to_string()];
    for t in &cmd.target { args.extend(["-target".to_string(), t.clone()]); }
    if cmd.no_input { args.push("-input=false".to_string()); }
    if cmd.no_lock { args.push("-lock=false".to_string()); }
    if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
    for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
    for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
    args
}

fn show_args(cmd: &cli::ShowCommand) -> Vec<String> {
    let mut args = vec!["show".to_string()];
    if cmd.json { args.push("-json".to_string()); }
    if cmd.show_sensitive { args.push("-show-sensitive".to_string()); }
    if let Some(ref p) = cmd.plan { args.push(p.clone()); }
    args
}

fn taint_args(cmd: &cli::TaintCommand) -> Vec<String> {
    let mut args = vec!["taint".to_string()];
    if cmd.allow_missing { args.push("-allow-missing".to_string()); }
    args.push(cmd.address.clone());
    args
}

fn untaint_args(cmd: &cli::UntaintCommand) -> Vec<String> {
    let mut args = vec!["untaint".to_string()];
    if cmd.allow_missing { args.push("-allow-missing".to_string()); }
    args.push(cmd.address.clone());
    args
}

fn test_args(cmd: &cli::TestCommand) -> Vec<String> {
    let mut args = vec!["test".to_string()];
    for f in &cmd.filter { args.extend(["-filter".to_string(), f.clone()]); }
    if cmd.json { args.push("-json".to_string()); }
    if cmd.no_color { args.push("-no-color".to_string()); }
    args
}

fn version_args(_cmd: &cli::VersionCommand) -> Vec<String> {
    vec!["version".to_string()]
}

fn workspace_args(sub: &cli::WorkspaceSubCommand) -> Vec<String> {
    match sub {
        cli::WorkspaceSubCommand::List(_) => vec!["workspace".to_string(), "list".to_string()],
        cli::WorkspaceSubCommand::Show(_) => vec!["workspace".to_string(), "show".to_string()],
        cli::WorkspaceSubCommand::New(cmd) => {
            let mut args = vec!["workspace".to_string(), "new".to_string()];
            if cmd.no_lock { args.push("-lock=false".to_string()); }
            if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
            args.push(cmd.name.clone());
            args
        }
        cli::WorkspaceSubCommand::Select(cmd) => {
            let mut args = vec!["workspace".to_string(), "select".to_string()];
            if cmd.no_lock { args.push("-lock=false".to_string()); }
            if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
            args.push(cmd.name.clone());
            args
        }
        cli::WorkspaceSubCommand::Delete(cmd) => {
            let mut args = vec!["workspace".to_string(), "delete".to_string()];
            if cmd.force { args.push("-force".to_string()); }
            if cmd.no_lock { args.push("-lock=false".to_string()); }
            if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
            args.push(cmd.name.clone());
            args
        }
    }
}

fn state_args(sub: &cli::StateSubCommand) -> Vec<String> {
    match sub {
        cli::StateSubCommand::List(cmd) => {
            let mut args = vec!["state".to_string(), "list".to_string()];
            if let Some(ref s) = cmd.state { args.extend(["-state".to_string(), s.clone()]); }
            if let Some(ref id) = cmd.id { args.extend(["-id".to_string(), id.clone()]); }
            for v in &cmd.var { args.extend(["-var".to_string(), v.clone()]); }
            for vf in &cmd.var_file { args.extend(["-var-file".to_string(), vf.clone()]); }
            for a in &cmd.addresses { args.push(a.clone()); }
            args
        }
        cli::StateSubCommand::Show(cmd) => {
            let mut args = vec!["state".to_string(), "show".to_string()];
            if let Some(ref s) = cmd.state { args.extend(["-state".to_string(), s.clone()]); }
            args.push(cmd.address.clone());
            args
        }
        cli::StateSubCommand::MV(cmd) => {
            let mut args = vec!["state".to_string(), "mv".to_string()];
            if let Some(ref s) = cmd.state { args.extend(["-state".to_string(), s.clone()]); }
            if let Some(ref so) = cmd.state_out { args.extend(["-state-out".to_string(), so.clone()]); }
            if cmd.no_lock { args.push("-lock=false".to_string()); }
            if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
            args.push(cmd.source.clone());
            args.push(cmd.destination.clone());
            args
        }
        cli::StateSubCommand::RM(cmd) => {
            let mut args = vec!["state".to_string(), "rm".to_string()];
            if let Some(ref s) = cmd.state { args.extend(["-state".to_string(), s.clone()]); }
            if cmd.no_lock { args.push("-lock=false".to_string()); }
            if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
            for a in &cmd.addresses { args.push(a.clone()); }
            args
        }
        cli::StateSubCommand::Pull(_) => vec!["state".to_string(), "pull".to_string()],
        cli::StateSubCommand::Push(_) => vec!["state".to_string(), "push".to_string()],
        cli::StateSubCommand::ReplaceProvider(cmd) => {
            let mut args = vec!["state".to_string(), "replace-provider".to_string()];
            if let Some(ref ms) = cmd.mirror_state { args.extend(["-mirror-state".to_string(), ms.clone()]); }
            if cmd.no_lock { args.push("-lock=false".to_string()); }
            if let Some(ref lt) = cmd.lock_timeout { args.extend(["-lock-timeout".to_string(), lt.clone()]); }
            args.push(cmd.old_provider.clone());
            args.push(cmd.new_provider.clone());
            args
        }
    }
}

fn metadata_args(_sub: &cli::MetadataSubCommand) -> Vec<String> {
    vec!["metadata".to_string()]
}

// ── Main dispatch ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli: cli::Cli = argh::from_env();
    let tofu_binary = tofu::TofuBinary::detect().ok();
    let terranix_binary = terranix::TerranixBinary::detect().ok();

    match cli.subcommand {
        // ── Tofu workflow commands ──
        cli::SubCommand::Init(ref cmd) => {
            auto_nix_generate(&terranix_binary);
            run_tofu(tofu_binary, init_args(cmd))
        }
        cli::SubCommand::Validate(ref cmd) => {
            auto_nix_generate(&terranix_binary);
            run_tofu(tofu_binary, validate_args(cmd))
        }
        cli::SubCommand::Plan(ref cmd) => {
            auto_nix_generate(&terranix_binary);
            if cmd.detail || cmd.json {
                run_tofu(tofu_binary, plan_args(cmd))
            } else {
                let mut args = plan_args(cmd);
                args.push("-json".to_string());
                args.push("-no-color".to_string());
                plan_json::run_plan_pretty(&require_tofu(tofu_binary), args)
            }
        }
        cli::SubCommand::Apply(ref cmd) => {
            auto_nix_generate(&terranix_binary);
            if cmd.detail || cmd.json {
                run_tofu(tofu_binary, apply_args(cmd))
            } else {
                plan_json::run_apply_pretty(&require_tofu(tofu_binary), cmd)
            }
        }
        cli::SubCommand::Destroy(ref cmd) => {
            auto_nix_generate(&terranix_binary);
            run_tofu(tofu_binary, destroy_args(cmd))
        }

        // ── Tofu utility commands ──
        cli::SubCommand::Console(cmd) => run_tofu(tofu_binary, console_args(&cmd)),
        cli::SubCommand::Fmt(cmd) => run_tofu(tofu_binary, fmt_args(&cmd)),
        cli::SubCommand::ForceUnlock(cmd) => run_tofu(tofu_binary, force_unlock_args(&cmd)),
        cli::SubCommand::Get(cmd) => run_tofu(tofu_binary, get_args(&cmd)),
        cli::SubCommand::Graph(cmd) => run_tofu(tofu_binary, graph_args(&cmd)),
        cli::SubCommand::Import(cmd) => run_tofu(tofu_binary, import_args(&cmd)),
        cli::SubCommand::Login(cmd) => run_tofu(tofu_binary, login_args(&cmd)),
        cli::SubCommand::Logout(cmd) => run_tofu(tofu_binary, logout_args(&cmd)),
        cli::SubCommand::Output(cmd) => run_tofu(tofu_binary, output_args(&cmd)),
        cli::SubCommand::Providers(cmd) => run_tofu(tofu_binary, providers_args(&cmd)),
        cli::SubCommand::Refresh(cmd) => run_tofu(tofu_binary, refresh_args(&cmd)),
        cli::SubCommand::Show(cmd) => run_tofu(tofu_binary, show_args(&cmd)),
        cli::SubCommand::Taint(cmd) => run_tofu(tofu_binary, taint_args(&cmd)),
        cli::SubCommand::Untaint(cmd) => run_tofu(tofu_binary, untaint_args(&cmd)),
        cli::SubCommand::Test(cmd) => run_tofu(tofu_binary, test_args(&cmd)),
        cli::SubCommand::Version(cmd) => run_tofu(tofu_binary, version_args(&cmd)),
        cli::SubCommand::Workspace(cmd) => run_tofu(tofu_binary, workspace_args(&cmd.subcommand)),
        cli::SubCommand::State(cmd) => run_tofu(tofu_binary, state_args(&cmd.subcommand)),
        cli::SubCommand::Metadata(cmd) => run_tofu(tofu_binary, metadata_args(&cmd.subcommand)),

        // ── Terranix commands ──
        cli::SubCommand::Nix(cmd) => match cmd.subcommand {
            cli::NixSubCommand::Generate(args) => {
                let terranix = match &terranix_binary {
                    Some(t) => t.clone(),
                    None => {
                        eprintln!("{} terranix binary not found in PATH.", "error:".bold().red());
                        eprintln!("Install it from https://terranix.org");
                        std::process::exit(1);
                    }
                };

                let mut terranix_args = Vec::new();
                for a in &args.arg {
                    terranix_args.push("--arg".to_string());
                    terranix_args.push(a.clone());
                }
                for a in &args.argstr {
                    terranix_args.push("--argstr".to_string());
                    terranix_args.push(a.clone());
                }
                terranix_args.push(args.config.clone());

                let output = std::process::Command::new(terranix.path())
                    .args(&terranix_args)
                    .output()
                    .unwrap_or_else(|e| {
                        eprintln!("{} Failed to execute terranix: {e}", "error:".bold().red());
                        std::process::exit(1);
                    });

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    eprintln!("{} terranix failed: {stderr}", "error:".bold().red());
                    std::process::exit(output.status.code().unwrap_or(1));
                }

                match args.output {
                    Some(ref out_path) => {
                        std::fs::write(out_path, &output.stdout).unwrap_or_else(|e| {
                            eprintln!("{} Failed to write {out_path}: {e}", "error:".bold().red());
                            std::process::exit(1);
                        });
                        println!("{} {} {}", "Generated".green(), out_path, "(from terranix)".dimmed());
                    }
                    None => {
                        std::io::stdout().write_all(&output.stdout).unwrap_or_else(|e| {
                            eprintln!("{} Failed to write to stdout: {e}", "error:".bold().red());
                            std::process::exit(1);
                        });
                    }
                }
            }
        },

        // ── Native terrarium commands ──
        cli::SubCommand::Serve(_) => serve(tofu_binary).await,

        cli::SubCommand::User(user_command) => {
            let users = authur::UserDB::new(data_dir().join("users").to_str().unwrap()).await;
            match user_command.subcommand {
                cli::UserCommands::Add(args) => {
                    let pass = args.password.unwrap_or_else(|| readline("Password: "));
                    users.create(args.username, &pass, Roles::default()).await.unwrap();
                }
                cli::UserCommands::ChangePassword(args) => {
                    if users.find(&args.username).await.is_some() {
                        let old_pass = readline("Current password: ");
                        let new_pass = args.password.unwrap_or_else(|| readline("New password: "));
                        match users.passwd(&args.username, &old_pass, &new_pass).await {
                            Ok(()) => println!("{}", "Password changed successfully".green()),
                            Err(()) => eprintln!("{}", "Error: incorrect current password".red()),
                        }
                    } else {
                        eprintln!("{}", "Error: unknown user".red());
                    }
                }
                cli::UserCommands::Delete(args) => {
                    if users.find(&args.username).await.is_some() {
                        let path = data_dir().join("users").join("users").join(&args.username);
                        match std::fs::remove_file(&path) {
                            Ok(()) => println!("User {} deleted", args.username.bold()),
                            Err(e) => eprintln!("{} {e}", "Error deleting user:".red()),
                        }
                    } else {
                        eprintln!("{}", "Error: unknown user".red());
                    }
                }
                cli::UserCommands::List(_) => {
                    let users = users.find_all().await;
                    println!("{}:", "Users".bold());
                    for u in users {
                        println!("  {u}");
                    }
                }
            }
        }

        cli::SubCommand::TerrariumLogin(login) => {
            let config_path = login
                .config
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var("TERRARIUM_CONFIG").ok().map(std::path::PathBuf::from))
                .or_else(|| dirs::config_dir().map(|d| d.join("terrarium").join("config.toml")))
                .unwrap_or_else(|| std::path::PathBuf::from(".terrarium.toml"));

            println!("Logging in — credentials will be saved to {:?}", config_path);
            let url = readline("Server URL (e.g. https://terra.example): ");
            let username = readline("Username: ");
            let password = rpassword::prompt_password("Password: ").unwrap_or_else(|_| readline("Password: "));

            match config::write(&config_path, &url, &username, &password) {
                Ok(()) => println!("{} {:?} {}", "Saved to".green(), config_path, "(chmod 600)".dimmed()),
                Err(e) => die(e),
            }
        }

        cli::SubCommand::Remote(remote) => {
            handle_remote_command(remote).await;
        }
    }
}

// ── Helper functions ─────────────────────────────────────────────────────

fn require_tofu(tofu_binary: Option<TofuBinary>) -> TofuBinary {
    match tofu_binary {
        Some(t) => t,
        None => {
            eprintln!("{} OpenTofu binary not found in PATH.", "error:".bold().red());
            eprintln!("Install it from https://opentofu.org");
            std::process::exit(1);
        }
    }
}

fn run_tofu(tofu_binary: Option<TofuBinary>, args: Vec<String>) -> ! {
    let tofu = require_tofu(tofu_binary);

    let status = tofu.run(&args.iter().map(|s| s.as_str()).collect::<Vec<&str>>())
        .unwrap_or_else(|e| {
            eprintln!("{} Failed to execute tofu: {e}", "error:".bold().red());
            std::process::exit(1);
        });

    std::process::exit(status.code().unwrap_or(1));
}

fn auto_nix_generate(terranix_binary: &Option<TerranixBinary>) {
    let candidates = [std::path::PathBuf::from("config.nix"), std::path::PathBuf::from("terra.nix")];
    let nix_path = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p.clone(),
        None => return,
    };

    let terranix = match terranix_binary {
        Some(t) => t.clone(),
        None => {
            eprintln!("{} Found {} but terranix is not installed.", "warning:".bold().yellow(), nix_path.display());
            eprintln!("  Install it from https://terranix.org or remove {}", nix_path.display());
            std::process::exit(1);
        }
    };

    let out_path = std::path::PathBuf::from("config.tf.json");

    let output = std::process::Command::new(terranix.path())
        .arg(nix_path.to_str().unwrap())
        .output()
        .unwrap_or_else(|e| {
            eprintln!("{} Failed to execute terranix: {e}", "error:".bold().red());
            std::process::exit(1);
        });

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{} terranix failed: {stderr}", "error:".bold().red());
        std::process::exit(output.status.code().unwrap_or(1));
    }

    std::fs::write(&out_path, &output.stdout).unwrap_or_else(|e| {
        eprintln!("{} Failed to write {}: {e}", "error:".bold().red(), out_path.display());
        std::process::exit(1);
    });

    eprintln!("{} config.tf.json {} {}", "↻".cyan(), "generated from".dimmed(), nix_path.display().to_string().bold().cyan());
}

fn readline(prompt: &str) -> String {
    print!("{}", prompt);
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    input.trim_end().to_string()
}

fn die(msg: String) -> ! {
    eprintln!("{} {msg}", "error:".bold().red());
    std::process::exit(1);
}

async fn serve(tofu_binary: Option<TofuBinary>) {
    let data = data_dir();
    let metrics_enabled = observability::enabled();
    if metrics_enabled {
        observability::init();
    }
    let state = AppState {
        state: StateContainer::new(data.join("state"), data.join("versions")),
        locks: LockContainer::new(data.join("locks")),
        users: authur::UserDB::new(data.join("users").to_str().unwrap()).await,
        webhooks: WebhookStore::new(data.join("webhooks.json")),
        tofu: tofu_binary,
        registry: registry::RegistryStore::new(data.join("registry")),
        mirror_status: std::sync::Arc::new(tokio::sync::RwLock::new(registry::MirrorStatus::default())),
    };

    let mut app = Router::new()
        // ── Terraform state / lock API ──
        .route("/state", get(state::list_states))
        .route(
            "/state/{*name}",
            get(state::get_state)
                .post(state::put_state)
                .delete(state::delete_state),
        )
        .route("/archive/{*name}", post(state::archive_state).delete(state::unarchive_state))
        .route("/versions/{*name}", get(state::list_versions))
        .route("/lock", get(lock::list_locks))
        .route("/lock/{*name}", post(lock::lock).delete(lock::unlock).fallback(lock::lock_method_compat))
        .route("/user/password", put(user::change_own_password))
        .route(
            "/webhooks/{*workspace}",
            get(webhook::list_webhooks).post(webhook::add_webhook),
        )
        .route("/webhooks/id/{id}", axum::routing::delete(webhook::remove_webhook))
        // ── Provider registry ──
        .route("/.well-known/terraform.json", get(registry::service_discovery))
        .route("/registry/v1/providers/{namespace}/{type}/versions", get(registry::list_versions))
        .route("/registry/v1/providers/{namespace}/{type}/{version}/download/{os}/{arch}", get(registry::download_info))
        .route("/registry/providers/{namespace}/{type}/{version}/{os}/{arch}", post(registry::upload_provider))
        .route("/registry/providers/{namespace}/{type}/{version}/{os}/{arch}/zip", get(registry::serve_binary))
        .route("/registry/providers/{namespace}/{type}/{version}/docs", axum::routing::put(registry::upload_docs))
        .route("/registry/mirror", post(registry::mirror_upstream))
        .route("/registry/mirror/{*path}", get(registry::network_mirror))
        .route("/registry/status", get(registry::registry_status))
        // ── Web UI ──
        .route("/", get(ui::dashboard))
        .route("/login", get(ui::login_page).post(ui::login_submit))
        .route("/logout", post(ui::logout))
        .route("/w/{*name}", get(ui::workspace))
        .route("/diff/{*name}", get(ui::diff_view))
        .route("/graph/{*name}", get(ui::graph_view))
        .route("/tokens", get(ui::tokens_page).post(ui::token_create))
        .route("/tokens/{id}/revoke", post(ui::token_revoke))
        .route("/registry", get(ui::registry_page))
        .route("/registry/{namespace}/{type}", get(ui::provider_page))
        .route("/registry/{namespace}/{type}/{version}/docs", get(ui::provider_docs_index))
        .route("/registry/{namespace}/{type}/{version}/docs/{*path}", get(ui::provider_doc_page))
        .route("/help", get(ui::help_page))
        .with_state(state.clone())
        .layer(middleware::from_fn(observability::http_middleware));

    if metrics_enabled {
        app = app.route("/metrics", get(observability::metrics));
        observability::spawn_collector(state.clone(), data.clone());
    }

    // Auto-mirror: spawn a background task if mirrors.json exists.
    let mirrors_path = data.join("mirrors.json");
    if mirrors_path.exists() {
        let interval_secs: Option<u64> = std::env::var("TERRARIUM_MIRROR_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n: &u64| n > 0);
        let state_clone = state;
        let path_clone  = mirrors_path.clone();
        tokio::spawn(async move {
            // Small delay so the server is fully up before the first mirror run.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Initial sync with backoff retries on transient failures.
            let mut errs = registry::run_auto_mirrors(state_clone.clone(), path_clone.clone()).await;
            for delay in [30u64, 120] {
                if errs == 0 { break; }
                tracing::warn!("Mirror had errors, retrying in {delay}s");
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                errs = registry::run_auto_mirrors(state_clone.clone(), path_clone.clone()).await;
            }

            if let Some(secs) = interval_secs {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
                // Skip missed ticks rather than letting a slow run pile up
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                ticker.tick().await; // consume the immediate first tick
                loop {
                    ticker.tick().await;
                    registry::run_auto_mirrors(state_clone.clone(), path_clone.clone()).await;
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("Failed to bind to port 8080");

    tracing::info!("🌱 Starting terra server at :8080");
    axum::serve(listener, app).await.unwrap();
}

async fn handle_remote_command(remote: cli::RemoteCommand) {
    let config = config::load(remote.config.map(std::path::PathBuf::from));
    let config = match config {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {e}", "configuration error:".bold().red());
            std::process::exit(1);
        }
    };
    let client = client::TerrariumClient::new(config.url, config.username, config.password);

    match remote.subcommand {
        cli::RemoteSubCommand::State(state_cmd) => match state_cmd.subcommand {
            cli::RemoteStateSubCommand::List(args) => {
                match client.list_states(args.prefix.as_deref(), args.archived).await {
                    Ok(states) if states.is_empty() => {
                        let msg = if args.archived { "No archived states found" } else { "No states found" };
                        println!("{}", msg.dimmed());
                    }
                    Ok(states) => {
                        let header = if args.archived {
                            "Archived states:".bold().yellow().to_string()
                        } else {
                            "States:".bold().to_string()
                        };
                        println!("{header}");
                        for s in states {
                            println!("  {s}");
                        }
                    }
                    Err(e) => die(e),
                }
            }
            cli::RemoteStateSubCommand::Get(args) => {
                match client.get_state(&args.name, args.version).await {
                    Ok(data) if args.raw => print!("{}", String::from_utf8_lossy(&data)),
                    Ok(data) => {
                        let s = String::from_utf8_lossy(&data);
                        match facet_json::from_str::<crate::tfstate::TfState>(&s) {
                            Ok(state) => {
                                let version_label = args.version.map(|v| format!(" v{v}")).unwrap_or_default();
                                println!("{} {}{}", "state    ".dimmed(), args.name.bold(), version_label.dimmed());
                                println!("{} {}", "terraform".dimmed(), state.terraform_version.cyan());
                                println!("{} {}", "serial   ".dimmed(), state.serial.to_string().cyan());
                                println!("{} {}", "lineage  ".dimmed(), state.lineage.dimmed());

                                if !state.resources.is_empty() {
                                    let mut by_type: std::collections::BTreeMap<String, Vec<String>> = Default::default();
                                    for r in &state.resources {
                                        let type_key = if r.mode == "data" { format!("data.{}", r.type_) } else { r.type_.clone() };
                                        let instance_name = match &r.module {
                                            Some(m) => format!("{}.{}", m, r.name),
                                            None => r.name.clone(),
                                        };
                                        by_type.entry(type_key).or_default().push(instance_name);
                                    }
                                    println!("\n{} {}:", "resources".bold(), state.resources.len().to_string().cyan());
                                    for (type_name, names) in &by_type {
                                        if names.len() > 1 {
                                            println!("  {} {}", type_name.bold().cyan(), format!("({})", names.len()).dimmed());
                                        } else {
                                            println!("  {}", type_name.bold().cyan());
                                        }
                                        for name in names {
                                            println!("    {name}");
                                        }
                                    }
                                }
                                if !state.outputs.is_empty() {
                                    println!("\n{}:", "outputs".bold());
                                    let mut keys: Vec<_> = state.outputs.keys().collect();
                                    keys.sort();
                                    for k in keys {
                                        let out = &state.outputs[k];
                                        if out.sensitive {
                                            println!("  {} {}", k, "(sensitive)".yellow().dimmed());
                                        } else {
                                            println!("  {k}");
                                        }
                                    }
                                }
                            }
                            Err(_) => {
                                match serde_json::from_slice::<serde_json::Value>(&data) {
                                    Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                                    Err(_) => print!("{s}"),
                                }
                            }
                        }
                    }
                    Err(e) => die(e),
                }
            }
            cli::RemoteStateSubCommand::Versions(args) => {
                match client.list_versions(&args.name).await {
                    Ok(versions) if versions.is_empty() => println!("{}", "No versions found".dimmed()),
                    Ok(versions) => {
                        println!("{} {}:", "Versions for".dimmed(), args.name.bold());
                        let current = versions.iter().max().copied();
                        for v in &versions {
                            if Some(*v) == current {
                                println!("  {} {}", v.to_string().cyan(), "(current)".green().bold());
                            } else {
                                println!("  {}", v.to_string().dimmed());
                            }
                        }
                    }
                    Err(e) => die(e),
                }
            }
            cli::RemoteStateSubCommand::Diff(args) => {
                let from = match client.get_state(&args.name, Some(args.from)).await {
                    Ok(d) => d,
                    Err(e) => die(e),
                };
                let to = match client.get_state(&args.name, Some(args.to)).await {
                    Ok(d) => d,
                    Err(e) => die(e),
                };

                use crate::statediff::{Change, StateDiff};

                match crate::statediff::diff_states(&from, &to) {
                    StateDiff::Structured { terraform_version, serial, changes } => {
                        println!("{} {} {} {} {}", "diff".bold(), args.name.bold(), format!("v{}", args.from).cyan(), "→".dimmed(), format!("v{}", args.to).cyan());

                        let no_changes = terraform_version.is_none() && serial.is_none() && changes.is_empty();

                        if let Some((a, b)) = &terraform_version {
                            println!("  {} {} {} {}", "terraform:".dimmed(), a.yellow(), "→".dimmed(), b.yellow());
                        }
                        if let Some((a, b)) = &serial {
                            println!("  {} {} {} {}", "serial:".dimmed(), a.to_string().yellow(), "→".dimmed(), b.to_string().yellow());
                        }

                        for change in &changes {
                            match change {
                                Change::Added(addr) => println!("  {} {}", "+".bold().green(), addr.green()),
                                Change::Removed(addr) => println!("  {} {}", "-".bold().red(), addr.red()),
                                Change::Modified { addr, instances } => {
                                    println!("  {} {}", "~".bold().yellow(), addr.yellow());
                                    for inst in instances {
                                        if inst.multi {
                                            println!("    {}", format!("[{}]", inst.index).dimmed());
                                        }
                                        for line in inst.diff.lines() {
                                            println!("    {line}");
                                        }
                                    }
                                }
                            }
                        }

                        if no_changes {
                            println!("  {}", "(no changes)".dimmed());
                        }
                    }
                    StateDiff::Raw(out) => println!("{out}"),
                    StateDiff::Error(e) => die(e),
                }
            }
            cli::RemoteStateSubCommand::Unlock(args) => {
                match client.unlock_state(&args.name).await {
                        Ok(info) => println!("{} {} {} {}", "Unlocked".green(), args.name.bold(), "— lock ID:".dimmed(), info.ID.cyan()),
                    Err(e) => die(e),
                }
            }
            cli::RemoteStateSubCommand::Archive(args) => {
                match client.archive_state(&args.name).await {
                    Ok(()) => println!("{} {} {}", "Archived".yellow(), args.name.bold(), "— now read-only".dimmed()),
                    Err(e) => die(e),
                }
            }
            cli::RemoteStateSubCommand::Unarchive(args) => {
                match client.unarchive_state(&args.name).await {
                    Ok(()) => println!("{} {} {}", "Unarchived".green(), args.name.bold(), "— writes re-enabled".dimmed()),
                    Err(e) => die(e),
                }
            }
        },
        cli::RemoteSubCommand::Lock(lock_cmd) => match lock_cmd.subcommand {
            cli::RemoteLockSubCommand::List(_) => {
                match client.list_locks().await {
                    Ok(locks) if locks.is_empty() => println!("{}", "No active locks".dimmed()),
                    Ok(locks) => {
                        println!("{}:", "Active locks".bold());
                        let mut entries: Vec<_> = locks.into_iter().collect();
                        entries.sort_by(|a, b| a.0.cmp(&b.0));
                        for (name, info) in entries {
                            println!("  {} {} {} {} {}", name.bold(), "locked by".dimmed(), info.Who.as_deref().unwrap_or("unknown").yellow(), "ID:".dimmed(), info.ID.cyan());
                        }
                    }
                    Err(e) => die(e),
                }
            }
        },
        cli::RemoteSubCommand::Webhook(webhook_cmd) => match webhook_cmd.subcommand {
            cli::RemoteWebhookSubCommand::Add(args) => {
                let events = args.events.unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
                match client.add_webhook(&args.workspace, &args.url, events).await {
                    Ok(hook) => println!("{} {}", "Webhook registered — ID:".green(), hook.id.cyan()),
                    Err(e) => die(e),
                }
            }
            cli::RemoteWebhookSubCommand::List(args) => {
                match client.list_webhooks(&args.workspace).await {
                    Ok(hooks) if hooks.is_empty() => println!("{} {}", "No webhooks for".dimmed(), args.workspace.bold()),
                    Ok(hooks) => {
                        println!("{} {}:", "Webhooks for".bold(), args.workspace.bold().cyan());
                        for h in hooks {
                            let events = if h.events.is_empty() {
                                "all events".dimmed().to_string()
                            } else {
                                h.events.join(", ").dimmed().to_string()
                            };
                            println!("  {} {} {} {}{}{}", h.id.cyan(), "→".dimmed(), h.url, "(".dimmed(), events, ")".dimmed());
                        }
                    }
                    Err(e) => die(e),
                }
            }
            cli::RemoteWebhookSubCommand::Remove(args) => {
                match client.remove_webhook(&args.id).await {
                    Ok(()) => println!("{} {}", "Webhook removed:".green(), args.id.cyan()),
                    Err(e) => die(e),
                }
            }
        },
        cli::RemoteSubCommand::User(user_cmd) => match user_cmd.subcommand {
            cli::RemoteUserSubCommand::Passwd(args) => {
                let new_pass = args.password.unwrap_or_else(|| readline("New password: "));
                match client.change_password(&new_pass).await {
                    Ok(()) => println!("{}", "Password changed successfully".green()),
                    Err(e) => die(e),
                }
            }
        },
    }
}
