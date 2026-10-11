//! The UI's few workspace actions: force-unlock, archive, unarchive.
//!
//! Each goes through the same handler as the HTTP API, so webhooks, metrics
//! and logging are identical no matter where the action came from. All are
//! CSRF-protected; tokens are single-use, so htmx responses carry the next
//! one in `X-CSRF-Token` for `app.js` to hand to every form on the page.

use authur::MaybeUser;
use authur::csrf::CSRF;
use axum::Form;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use maud::html;

use super::components::icon;
use super::layout::{Section, Shell, render};
use super::workspace::lock_card;
use super::{go, is_htmx, require_login, valid_name};
use crate::AppState;
use crate::auth::AuthUser;

#[derive(serde::Deserialize)]
pub struct CsrfForm {
    csrf: String,
}

/// Validates the request; `Err` is the response to send instead.
async fn check(app: &AppState, maybe: MaybeUser, name: &str, csrf: &str, action: &'static str) -> Result<AuthUser, Response> {
    let MaybeUser::User(user) = maybe else {
        return Err(require_login());
    };
    if !valid_name(name) {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    if !app.users.verify_csrf(csrf, &user.username).await {
        metrics::counter!("terrarium_auth_csrf_failures_total", "action" => action).increment(1);
        let shell = Shell::new(app, Some(&user.username), Section::Workspace).at(name);
        return Err((
            StatusCode::FORBIDDEN,
            render(&shell, "Expired form", html! {
                div.head { div.head-text { h1 { "That form expired" } } }
                section.card { div.card-b {
                    p style="margin-top:0" { "Forms are single-use and this one was already spent — probably in another tab. Nothing was changed." }
                    a.btn href={"/w/" (name)} { (icon("arrow")) "Back to " span.mono { (name) } }
                } }
            }),
        )
            .into_response());
    }
    Ok(AuthUser(user))
}

fn toast(msg: &str, ok: bool) -> String {
    serde_json::json!({ "toast": { "msg": msg, "kind": if ok { "ok" } else { "err" } } }).to_string()
}

/// `POST /ui/unlock/{name}` — force-release whatever lock is held.
pub async fn force_unlock(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> Response {
    let user = match check(&app, maybe, &name, &f.csrf, "force_unlock").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let username = user.0.username.clone();
    tracing::info!("🔓 {username} force-unlocking {name} from the web UI");
    let status = crate::lock::unlock(user, State(app.clone()), Path(name.clone()), Bytes::new()).await.status();

    if !is_htmx(&headers) {
        return go(&headers, &format!("/w/{name}"));
    }
    let msg = match status {
        s if s.is_success() => toast("Lock released", true),
        StatusCode::NOT_FOUND => toast("Already unlocked", true),
        _ => toast("Could not release the lock", false),
    };
    let csrf = app.users.get_csrf(&username).await;
    (
        [("X-CSRF-Token", csrf.clone()), ("HX-Trigger", msg)],
        lock_card(&app, &name, &csrf),
    )
        .into_response()
}

/// `POST /ui/archive/{name}`
pub async fn archive(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> Response {
    let user = match check(&app, maybe, &name, &f.csrf, "archive").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let _ = crate::state::archive_state(State(app), Path(name.clone()), user).await;
    go(&headers, &format!("/w/{name}"))
}

/// `POST /ui/unarchive/{name}`
pub async fn unarchive(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> Response {
    let user = match check(&app, maybe, &name, &f.csrf, "unarchive").await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let _ = crate::state::unarchive_state(State(app), Path(name.clone()), user).await;
    go(&headers, &format!("/w/{name}"))
}
