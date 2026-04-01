use std::io::Write as _;

use authur::Roles;
use axum::{
    Router,
    routing::{get, post, put},
};
use colored::Colorize as _;

use crate::{lock::LockContainer, state::StateContainer, webhook::WebhookStore};

mod cli;
mod client;
mod config;
pub mod lock;
pub mod state;
pub mod tfstate;
pub mod user;
pub mod webhook;

#[derive(Clone)]
pub struct AppState {
    state: state::StateContainer,
    locks: lock::LockContainer,
    users: authur::UserDB<authur::vfs::PhysicalFS>,
    webhooks: webhook::WebhookStore,
}

impl axum::extract::FromRef<AppState> for authur::UserDB<authur::vfs::PhysicalFS> {
    fn from_ref(input: &AppState) -> Self {
        input.users.clone()
    }
}

/// Returns the data directory, honoring TERRARIUM_DATA env var.
/// Defaults to "." (relative to process CWD), which is /app inside the container.
/// Set TERRARIUM_DATA=/ to use old absolute-path volume layout (/state, /users, /locks).
fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var("TERRARIUM_DATA").unwrap_or_else(|_| ".".to_string()),
    )
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli: cli::Cli = argh::from_env();

    match cli.subcommand {
        cli::SubCommand::Serve(_) => serve().await,

        cli::SubCommand::User(user_command) => {
            let users = authur::UserDB::new(data_dir().join("users").to_str().unwrap()).await;

            match user_command.subcommand {
                cli::UserCommands::Add(args) => {
                    let pass = args.password.unwrap_or_else(|| readline("Password: "));
                    users
                        .create(args.username, &pass, Roles::default())
                        .await
                        .unwrap();
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
                        // TODO: implement user deletion in authur upstream, then replace this
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

        cli::SubCommand::Login(login) => {
            let config_path = login
                .config
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var("TERRARIUM_CONFIG").ok().map(std::path::PathBuf::from))
                .or_else(|| dirs::config_dir().map(|d| d.join("terrarium").join("config.toml")))
                .unwrap_or_else(|| std::path::PathBuf::from(".terrarium.toml"));

            println!("Logging in — credentials will be saved to {:?}", config_path);

            let url = readline("Server URL (e.g. https://terrarium.example): ");
            let username = readline("Username: ");
            let password = rpassword::prompt_password("Password: ").unwrap_or_else(|_| readline("Password: "));

            match config::write(&config_path, &url, &username, &password) {
                Ok(()) => println!("{} {:?} {}", "Saved to".green(), config_path, "(chmod 600)".dimmed()),
                Err(e) => die(e),
            }
        }

        cli::SubCommand::Remote(remote) => {
            let config = config::load(remote.config.map(std::path::PathBuf::from));
            let config = match config {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{} {e}", "configuration error:".bold().red());
                    std::process::exit(1);
                }
            };
            let client =
                client::TerrariumClient::new(config.url, config.username, config.password);

            match remote.subcommand {
                cli::RemoteSubCommand::State(state_cmd) => match state_cmd.subcommand {
                    cli::RemoteStateSubCommand::List(args) => {
                        match client.list_states(args.prefix.as_deref(), args.archived).await {
                            Ok(states) if states.is_empty() => {
                                let msg = if args.archived {
                                    "No archived states found"
                                } else {
                                    "No states found"
                                };
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
                            Ok(data) if args.raw => {
                                print!("{}", String::from_utf8_lossy(&data));
                            }
                            Ok(data) => {
                                let s = String::from_utf8_lossy(&data);
                                match facet_json::from_str::<crate::tfstate::TfState>(&s) {
                                    Ok(state) => {
                                        let version_label = args.version
                                            .map(|v| format!(" v{v}"))
                                            .unwrap_or_default();
                                        println!("{} {}{}",
                                            "state    ".dimmed(),
                                            args.name.bold(),
                                            version_label.dimmed()
                                        );
                                        println!("{} {}",
                                            "terraform".dimmed(),
                                            state.terraform_version.cyan()
                                        );
                                        println!("{} {}",
                                            "serial   ".dimmed(),
                                            state.serial.to_string().cyan()
                                        );
                                        println!("{} {}",
                                            "lineage  ".dimmed(),
                                            state.lineage.dimmed()
                                        );

                                        if !state.resources.is_empty() {
                                            // Group by resource type
                                            let mut by_type: std::collections::BTreeMap<String, Vec<String>> =
                                                Default::default();
                                            for r in &state.resources {
                                                let type_key = if r.mode == "data" {
                                                    format!("data.{}", r.type_)
                                                } else {
                                                    r.type_.clone()
                                                };
                                                let instance_name = match &r.module {
                                                    Some(m) => format!("{}.{}", m, r.name),
                                                    None => r.name.clone(),
                                                };
                                                by_type.entry(type_key).or_default().push(instance_name);
                                            }

                                            println!("\n{} {}:",
                                                "resources".bold(),
                                                state.resources.len().to_string().cyan()
                                            );
                                            for (type_name, names) in &by_type {
                                                if names.len() > 1 {
                                                    println!("  {} {}",
                                                        type_name.bold().cyan(),
                                                        format!("({})", names.len()).dimmed()
                                                    );
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
                                                    println!("  {} {}",
                                                        k,
                                                        "(sensitive)".yellow().dimmed()
                                                    );
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
                            Ok(versions) if versions.is_empty() => {
                                println!("{}", "No versions found".dimmed());
                            }
                            Ok(versions) => {
                                println!("{} {}:",
                                    "Versions for".dimmed(),
                                    args.name.bold()
                                );
                                let current = versions.iter().max().copied();
                                for v in &versions {
                                    if Some(*v) == current {
                                        println!("  {} {}",
                                            v.to_string().cyan(),
                                            "(current)".green().bold()
                                        );
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

                        use facet_diff::FacetDiff;
                        use std::collections::HashMap as AddrMap;

                        let from_s = String::from_utf8_lossy(&from);
                        let to_s = String::from_utf8_lossy(&to);

                        let a_state = facet_json::from_str::<crate::tfstate::TfState>(&from_s).ok();
                        let b_state = facet_json::from_str::<crate::tfstate::TfState>(&to_s).ok();

                        if let (Some(a), Some(b)) = (a_state, b_state) {
                            let a_map: AddrMap<String, &crate::tfstate::TfResource> =
                                a.resources.iter().map(|r| (r.address(), r)).collect();
                            let b_map: AddrMap<String, &crate::tfstate::TfResource> =
                                b.resources.iter().map(|r| (r.address(), r)).collect();

                            let mut addrs: Vec<String> =
                                a_map.keys().chain(b_map.keys()).cloned().collect();
                            addrs.sort();
                            addrs.dedup();

                            println!("{} {} {} {} {}",
                                "diff".bold(),
                                args.name.bold(),
                                format!("v{}", args.from).cyan(),
                                "→".dimmed(),
                                format!("v{}", args.to).cyan(),
                            );
                            let mut any_change = false;

                            if a.serial != b.serial || a.terraform_version != b.terraform_version {
                                if a.terraform_version != b.terraform_version {
                                    println!("  {} {} {} {}",
                                        "terraform:".dimmed(),
                                        a.terraform_version.yellow(),
                                        "→".dimmed(),
                                        b.terraform_version.yellow()
                                    );
                                }
                                println!("  {} {} {} {}",
                                    "serial:".dimmed(),
                                    a.serial.to_string().yellow(),
                                    "→".dimmed(),
                                    b.serial.to_string().yellow()
                                );
                                any_change = true;
                            }

                            for addr in &addrs {
                                match (a_map.get(addr), b_map.get(addr)) {
                                    (None, Some(_)) => {
                                        println!("  {} {}", "+".bold().green(), addr.green());
                                        any_change = true;
                                    }
                                    (Some(_), None) => {
                                        println!("  {} {}", "-".bold().red(), addr.red());
                                        any_change = true;
                                    }
                                    (Some(ra), Some(rb)) if ra.instances != rb.instances => {
                                        println!("  {} {}", "~".bold().yellow(), addr.yellow());
                                        for (i, (ia, ib)) in ra.instances.iter().zip(rb.instances.iter()).enumerate() {
                                            if ia.attributes != ib.attributes {
                                                if ra.instances.len() > 1 {
                                                    println!("    {}", format!("[{i}]").dimmed());
                                                }
                                                let diff = format!("{}", ia.attributes.diff(&ib.attributes));
                                                for line in diff.lines() {
                                                    println!("    {line}");
                                                }
                                            }
                                        }
                                        any_change = true;
                                    }
                                    _ => {}
                                }
                            }

                            if !any_change {
                                println!("  {}", "(no changes)".dimmed());
                            }
                        } else {
                            // Non-TF state or unparseable — fall back to generic Value diff
                            let a = match facet_json::from_str::<facet_value::Value>(&from_s) {
                                Ok(v) => v,
                                Err(e) => die(format!("v{} is not valid JSON: {e}", args.from)),
                            };
                            let b = match facet_json::from_str::<facet_value::Value>(&to_s) {
                                Ok(v) => v,
                                Err(e) => die(format!("v{} is not valid JSON: {e}", args.to)),
                            };
                            println!("{}", a.diff(&b));
                        }
                    }
                    cli::RemoteStateSubCommand::Unlock(args) => {
                        match client.unlock_state(&args.name).await {
                            Ok(info) => println!("{} {} {} {}",
                                "Unlocked".green(),
                                args.name.bold(),
                                "— lock ID:".dimmed(),
                                info.ID.cyan()
                            ),
                            Err(e) => die(e),
                        }
                    }
                    cli::RemoteStateSubCommand::Archive(args) => {
                        match client.archive_state(&args.name).await {
                            Ok(()) => println!("{} {} {}",
                                "Archived".yellow(),
                                args.name.bold(),
                                "— now read-only".dimmed()
                            ),
                            Err(e) => die(e),
                        }
                    }
                    cli::RemoteStateSubCommand::Unarchive(args) => {
                        match client.unarchive_state(&args.name).await {
                            Ok(()) => println!("{} {} {}",
                                "Unarchived".green(),
                                args.name.bold(),
                                "— writes re-enabled".dimmed()
                            ),
                            Err(e) => die(e),
                        }
                    }
                },

                cli::RemoteSubCommand::Lock(lock_cmd) => match lock_cmd.subcommand {
                    cli::RemoteLockSubCommand::List(_) => {
                        match client.list_locks().await {
                            Ok(locks) if locks.is_empty() => {
                                println!("{}", "No active locks".dimmed());
                            }
                            Ok(locks) => {
                                println!("{}:", "Active locks".bold());
                                let mut entries: Vec<_> = locks.into_iter().collect();
                                entries.sort_by(|a, b| a.0.cmp(&b.0));
                                for (name, info) in entries {
                                    println!("  {} {} {} {} {}",
                                        name.bold(),
                                        "locked by".dimmed(),
                                        info.Who.as_deref().unwrap_or("unknown").yellow(),
                                        "ID:".dimmed(),
                                        info.ID.cyan()
                                    );
                                }
                            }
                            Err(e) => die(e),
                        }
                    }
                },

                cli::RemoteSubCommand::Webhook(webhook_cmd) => match webhook_cmd.subcommand {
                    cli::RemoteWebhookSubCommand::Add(args) => {
                        let events = args
                            .events
                            .unwrap_or_default()
                            .split(',')
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        match client.add_webhook(&args.workspace, &args.url, events).await {
                            Ok(hook) => println!("{} {}",
                                "Webhook registered — ID:".green(),
                                hook.id.cyan()
                            ),
                            Err(e) => die(e),
                        }
                    }
                    cli::RemoteWebhookSubCommand::List(args) => {
                        match client.list_webhooks(&args.workspace).await {
                            Ok(hooks) if hooks.is_empty() => {
                                println!("{} {}", "No webhooks for".dimmed(), args.workspace.bold());
                            }
                            Ok(hooks) => {
                                println!("{} {}:",
                                    "Webhooks for".bold(),
                                    args.workspace.bold().cyan()
                                );
                                for h in hooks {
                                    let events = if h.events.is_empty() {
                                        "all events".dimmed().to_string()
                                    } else {
                                        h.events.join(", ").dimmed().to_string()
                                    };
                                    println!("  {} {} {} {}{}{}",
                                        h.id.cyan(),
                                        "→".dimmed(),
                                        h.url,
                                        "(".dimmed(),
                                        events,
                                        ")".dimmed()
                                    );
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
    }
}

pub(crate) fn readline(prompt: &str) -> String {
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

async fn serve() {
    let data = data_dir();
    let state = AppState {
        state: StateContainer::new(data.join("state"), data.join("versions")),
        locks: LockContainer::new(data.join("locks")),
        users: authur::UserDB::new(data.join("users").to_str().unwrap()).await,
        webhooks: WebhookStore::new(data.join("webhooks.json")),
    };

    let app = Router::new()
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
        .route("/lock/{*name}", post(lock::lock).delete(lock::unlock))
        .route("/user/password", put(user::change_own_password))
        .route(
            "/webhooks/{*workspace}",
            get(webhook::list_webhooks).post(webhook::add_webhook),
        )
        .route("/webhooks/id/{id}", axum::routing::delete(webhook::remove_webhook))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("Failed to bind to port 8080");

    tracing::info!("🌱 Starting terrarium server at :8080");
    axum::serve(listener, app).await.unwrap();
}
