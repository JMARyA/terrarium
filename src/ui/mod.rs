//! Web UI served alongside the HTTP state API.
//!
//! Server-rendered with [maud] components, made interactive with a vendored
//! htmx (boosted navigation, live lock status, inline diffs, the command
//! palette) and a small `app.js`. Every asset is compiled into the binary.
//!
//! Browser auth uses a `session` cookie (authur [`authur::UserAuth`] /
//! [`authur::MaybeUser`]), completely separate from the Basic-Auth path the
//! Terraform backend uses. Every view reads from the same [`AppState`]
//! containers the API handlers use — no duplicated logic, no internal HTTP.
//!
//! The UI is mostly a viewer. The only things it changes are the viewer's own
//! session and tokens, plus two small workspace actions: force-unlocking a
//! lock and archiving/unarchiving — both routed through the API handlers so
//! they fire the same webhooks and metrics.

mod account;
mod actions;
mod ansi;
mod assets;
mod components;
mod data;
mod graph;
mod help;
mod history;
mod inspect;
mod layout;
mod overview;
mod policies;
mod registry;
mod search;
mod state_view;
mod workspace;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};

use crate::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(overview::page))
        .route("/login", get(account::login_page).post(account::login_submit))
        .route("/logout", post(account::logout))
        .route("/w/{*name}", get(workspace::page))
        .route("/graph/{*name}", get(state_view::page))
        .route("/diff/{*name}", get(history::page))
        .route("/tokens", get(account::tokens_page).post(account::token_create))
        .route("/tokens/{id}/revoke", post(account::token_revoke))
        .route("/policies", get(policies::page))
        .route("/registry", get(registry::index))
        .route("/registry/{namespace}/{type}", get(registry::provider))
        .route("/registry/{namespace}/{type}/{version}/docs", get(registry::docs_index))
        .route("/registry/{namespace}/{type}/{version}/docs/{*path}", get(registry::docs_page))
        .route("/help", get(help::page))
        .route("/static/{file}", get(assets::serve))
        // htmx fragments and actions
        .route("/ui/search", get(search::results))
        .route("/ui/locks", get(overview::locks_fragment))
        .route("/ui/lock/{*name}", get(workspace::lock_fragment))
        .route("/ui/resource/{*name}", get(inspect::fragment))
        .route("/ui/unlock/{*name}", post(actions::force_unlock))
        .route("/ui/archive/{*name}", post(actions::archive))
        .route("/ui/unarchive/{*name}", post(actions::unarchive))
}

// ── Shared helpers ──────────────────────────────────────────────────────

/// `; Secure` when terra runs behind TLS (set `TERRARIUM_TLS=1`). Off by
/// default so the cookie works over plain http during local testing.
fn tls() -> bool {
    matches!(std::env::var("TERRARIUM_TLS"), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Reject workspace names that could escape the data dir. Mirrors the
/// validation the state/lock API handlers apply.
fn valid_name(name: &str) -> bool {
    crate::state::validate_name(name).is_ok() && !name.contains('\\')
}

fn require_login() -> Response {
    Redirect::to("/login").into_response()
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("HX-Request")
}

/// The origin this server is reached at, for copy-pasteable snippets.
/// Derived from the request so examples are right behind any proxy.
fn origin(headers: &HeaderMap) -> String {
    let host = headers
        .get("X-Forwarded-Host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|h| h.to_str().ok())
        .filter(|h| h.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)))
        .unwrap_or("terrarium.example");
    let proto = headers
        .get("X-Forwarded-Proto")
        .and_then(|h| h.to_str().ok())
        .filter(|p| *p == "http" || *p == "https")
        .unwrap_or(if tls() { "https" } else { "http" });
    format!("{proto}://{host}")
}

/// Navigate after a form post. htmx gets `HX-Location`, so the address bar
/// shows the destination rather than the endpoint that was posted to.
fn go(headers: &HeaderMap, path: &str) -> Response {
    if is_htmx(headers) {
        (StatusCode::OK, [("HX-Location", path.to_string())]).into_response()
    } else {
        Redirect::to(path).into_response()
    }
}

fn not_found(page: maud::Markup) -> Response {
    (StatusCode::NOT_FOUND, page).into_response()
}
