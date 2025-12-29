use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::{lock::LockContainer, state::StateContainer};

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

    let state = AppState {
        state: StateContainer::new("./state".into()),
        locks: LockContainer::new("./locks".into()),
        users: authur::UserDB::new("./users").await,
    };

    let app = Router::new()
        .route(
            "/state/:name",
            get(state::get_state)
                .post(state::put_state)
                .delete(state::delete_state),
        )
        .route("/lock/:name", post(lock::lock))
        .route("/lock/:name", delete(lock::unlock))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("Failed to bind to port 8080");

    tracing::info!("🌱 Starting terrarium server at :8080");
    axum::serve(listener, app).await.unwrap();
}
