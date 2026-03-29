use authur::extractor::BasicAuthUser;
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;

use crate::AppState;

#[derive(Deserialize)]
pub struct ChangePasswordBody {
    pub current_password: String,
    pub new_password: String,
}

/// Self-service password change — authenticated user changes their own password
pub async fn change_own_password(
    BasicAuthUser(user): BasicAuthUser,
    State(app): State<AppState>,
    Json(body): Json<ChangePasswordBody>,
) -> Result<StatusCode, StatusCode> {
    app.users
        .passwd(&user.username, &body.current_password, &body.new_password)
        .await
        .map(|_| StatusCode::OK)
        .map_err(|_| StatusCode::UNAUTHORIZED)
}
