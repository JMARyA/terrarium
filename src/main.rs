use std::io::Write;

use authur::Roles;
use axum::{
    Router,
    routing::{get, post, put},
};

use crate::{lock::LockContainer, state::StateContainer};

mod cli;
mod client;
mod config;
pub mod lock;
pub mod state;
pub mod user;

#[derive(Clone)]
pub struct AppState {
    state: state::StateContainer,
    locks: lock::LockContainer,
    users: authur::UserDB<authur::vfs::PhysicalFS>,
}

impl axum::extract::FromRef<AppState> for authur::UserDB<authur::vfs::PhysicalFS> {
    fn from_ref(input: &AppState) -> Self {
        input.users.clone()
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli: cli::Cli = argh::from_env();

    match cli.subcommand {
        cli::SubCommand::Serve(_) => serve().await,

        cli::SubCommand::User(user_command) => {
            let users = authur::UserDB::new("./users").await;

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
                            Ok(()) => println!("Password changed successfully"),
                            Err(()) => println!("Error: incorrect current password"),
                        }
                    } else {
                        println!("Error: unknown user");
                    }
                }
                cli::UserCommands::Delete(args) => {
                    if users.find(&args.username).await.is_some() {
                        // TODO: implement user deletion in authur upstream, then replace this
                        let path = format!("./users/users/{}", args.username);
                        match std::fs::remove_file(&path) {
                            Ok(()) => println!("User '{}' deleted", args.username),
                            Err(e) => println!("Error deleting user: {e}"),
                        }
                    } else {
                        println!("Error: unknown user");
                    }
                }
                cli::UserCommands::List(_) => {
                    let users = users.find_all().await;
                    println!("Users:");
                    for u in users {
                        println!("- {u}");
                    }
                }
            }
        }

        cli::SubCommand::Remote(remote) => {
            let config = config::load(remote.config.map(std::path::PathBuf::from));
            let config = match config {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Configuration error: {e}");
                    std::process::exit(1);
                }
            };
            let client =
                client::TerrariumClient::new(config.url, config.username, config.password);

            match remote.subcommand {
                cli::RemoteSubCommand::State(state_cmd) => match state_cmd.subcommand {
                    cli::RemoteStateSubCommand::List(args) => {
                        match client.list_states(args.prefix.as_deref()).await {
                            Ok(states) if states.is_empty() => println!("No states found"),
                            Ok(states) => {
                                println!("States:");
                                for s in states {
                                    println!("  - {s}");
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
                                match serde_json::from_slice::<serde_json::Value>(&data) {
                                    Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
                                    Err(_) => print!("{}", String::from_utf8_lossy(&data)),
                                }
                            }
                            Err(e) => die(e),
                        }
                    }
                    cli::RemoteStateSubCommand::Versions(args) => {
                        match client.list_versions(&args.name).await {
                            Ok(versions) if versions.is_empty() => println!("No versions found"),
                            Ok(versions) => {
                                println!("Versions for '{}':", args.name);
                                for v in versions {
                                    println!("  {v}");
                                }
                            }
                            Err(e) => die(e),
                        }
                    }
                    cli::RemoteStateSubCommand::Unlock(args) => {
                        match client.unlock_state(&args.name).await {
                            Ok(info) => {
                                println!("Unlocked '{}' (lock ID: {})", args.name, info.ID)
                            }
                            Err(e) => die(e),
                        }
                    }
                    cli::RemoteStateSubCommand::Archive(args) => {
                        match client.archive_state(&args.name).await {
                            Ok(()) => println!("Archived '{}' — now read-only", args.name),
                            Err(e) => die(e),
                        }
                    }
                },

                cli::RemoteSubCommand::Lock(lock_cmd) => match lock_cmd.subcommand {
                    cli::RemoteLockSubCommand::List(_) => {
                        match client.list_locks().await {
                            Ok(locks) if locks.is_empty() => println!("No active locks"),
                            Ok(locks) => {
                                println!("Active locks:");
                                let mut entries: Vec<_> = locks.into_iter().collect();
                                entries.sort_by(|a, b| a.0.cmp(&b.0));
                                for (name, info) in entries {
                                    println!(
                                        "  - {name}: locked by {} (ID: {})",
                                        info.Who.as_deref().unwrap_or("unknown"),
                                        info.ID
                                    );
                                }
                            }
                            Err(e) => die(e),
                        }
                    }
                },

                cli::RemoteSubCommand::User(user_cmd) => match user_cmd.subcommand {
                    cli::RemoteUserSubCommand::Passwd(args) => {
                        let new_pass = args.password.unwrap_or_else(|| readline("New password: "));
                        match client.change_password(&new_pass).await {
                            Ok(()) => println!("Password changed successfully"),
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

fn die(msg: String) {
    eprintln!("Error: {msg}");
    std::process::exit(1);
}

async fn serve() {
    let state = AppState {
        state: StateContainer::new("./state".into(), "./versions".into()),
        locks: LockContainer::new("./locks".into()),
        users: authur::UserDB::new("./users").await,
    };

    let app = Router::new()
        .route("/state", get(state::list_states))
        .route(
            "/state/{*name}",
            get(state::get_state)
                .post(state::put_state)
                .delete(state::delete_state),
        )
        .route("/archive/{*name}", post(state::archive_state))
        .route("/versions/{*name}", get(state::list_versions))
        .route("/lock", get(lock::list_locks))
        .route("/lock/{*name}", post(lock::lock).delete(lock::unlock))
        .route("/user/password", put(user::change_own_password))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("Failed to bind to port 8080");

    tracing::info!("🌱 Starting terrarium server at :8080");
    axum::serve(listener, app).await.unwrap();
}
