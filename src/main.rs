use std::io::Write;

use authur::Roles;
use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::{lock::LockContainer, state::StateContainer};

mod cli;
pub mod lock;
pub mod state;

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
    }
}

fn readline(prompt: &str) -> String {
    print!("{}", prompt);
    std::io::stdout().flush().unwrap();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    input.trim_end().to_string()
}

async fn serve() {
    let state = AppState {
        state: StateContainer::new("./state".into()),
        locks: LockContainer::new("./locks".into()),
        users: authur::UserDB::new("./users").await,
    };

    let app = Router::new()
        .route(
            "/state/{name}",
            get(state::get_state)
                .post(state::put_state)
                .delete(state::delete_state),
        )
        .route("/lock/{name}", post(lock::lock))
        .route("/lock/{name}", delete(lock::unlock))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("Failed to bind to port 8080");

    tracing::info!("🌱 Starting terrarium server at :8080");
    axum::serve(listener, app).await.unwrap();
}
