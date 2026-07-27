use crate::auth::AuthUser;
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
    AuthUser(user): AuthUser,
    State(app): State<AppState>,
    Json(body): Json<ChangePasswordBody>,
) -> Result<StatusCode, StatusCode> {
    match app
        .users
        .passwd(&user.username, &body.current_password, &body.new_password)
        .await
    {
        Ok(_) => {
            metrics::counter!("terrarium_auth_password_changes_total", "result" => "ok").increment(1);
            Ok(StatusCode::OK)
        }
        Err(_) => {
            metrics::counter!("terrarium_auth_password_changes_total", "result" => "error").increment(1);
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}
