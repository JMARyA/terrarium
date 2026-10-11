//! Sign-in, sign-out, and the viewer's own sessions and API tokens.

use authur::csrf::CSRF;
use authur::{MaybeUser, Sessions, UserAuth, session_id};
use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, html};

use super::components::{csrf_input, empty, icon, logo, time};
use super::layout::{Section, Shell, bare, render};
use super::{go, tls};
use crate::AppState;

// ── Cookies ─────────────────────────────────────────────────────────────

fn set_cookie(token: &str) -> String {
    format!(
        "session={token}; HttpOnly{}; SameSite=Lax; Path=/; Max-Age=2592000",
        if tls() { "; Secure" } else { "" }
    )
}

fn clear_cookie() -> &'static str {
    "session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"
}

fn read_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .find_map(|kv| kv.trim().strip_prefix("session=").map(str::to_string))
}

// ── Sign in / out ───────────────────────────────────────────────────────

pub async fn login_page(maybe: MaybeUser) -> Response {
    if maybe.user().is_some() {
        return Redirect::to("/").into_response();
    }
    login_form(None, "").into_response()
}

fn login_form(error: Option<&str>, username: &str) -> Markup {
    bare("Sign in", html! {
        div.login {
            div.login-card {
                div.brand { (logo()) "terrarium" }
                p.tag { "A safe enclosure for your Terraform state." }
                div.card {
                    form method="post" action="/login" {
                        @if let Some(e) = error { div.alert.coral role="alert" { (e) } }
                        label.field {
                            "Username"
                            input type="text" name="username" value=(username) autocomplete="username"
                                autofocus[username.is_empty()] required;
                        }
                        label.field {
                            "Password"
                            input type="password" name="password" autocomplete="current-password"
                                autofocus[!username.is_empty()] required;
                        }
                        button.btn.primary type="submit" { "Sign in" }
                    }
                }
                p.note style="text-align:center;margin-top:16px" {
                    a.link href="/help" { "How to use terrarium" } " · " a.link href="/registry" { "Provider registry" }
                }
            }
        }
    })
}

#[derive(serde::Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

pub async fn login_submit(State(app): State<AppState>, Form(f): Form<LoginForm>) -> Response {
    match app.users.login(&f.username, &f.password).await {
        Some((session, _roles)) => {
            metrics::counter!("terrarium_auth_logins_total", "result" => "success").increment(1);
            ([(header::SET_COOKIE, set_cookie(&session.token))], Redirect::to("/")).into_response()
        }
        None => {
            metrics::counter!("terrarium_auth_logins_total", "result" => "failure").increment(1);
            (StatusCode::UNAUTHORIZED, login_form(Some("Wrong username or password."), &f.username)).into_response()
        }
    }
}

pub async fn logout(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = read_session_cookie(&headers) {
        app.users.end_session(&session_id(&token)).await;
    }
    ([(header::SET_COOKIE, clear_cookie())], Redirect::to("/login")).into_response()
}

// ── Tokens ──────────────────────────────────────────────────────────────

pub async fn tokens_page(UserAuth(user): UserAuth, State(app): State<AppState>, headers: HeaderMap) -> Response {
    render_tokens(&app, &user.username, &headers, None, None).await.into_response()
}

async fn render_tokens(
    app: &AppState,
    user: &str,
    headers: &HeaderMap,
    created: Option<(&str, &str)>,
    error: Option<&str>,
) -> Markup {
    let csrf = app.users.get_csrf(user).await;
    let mut sessions = app.users.list_sessions(user).await;
    sessions.sort_by(|a, b| b.created.cmp(&a.created));
    let this = read_session_cookie(headers).map(|t| session_id(&t));
    let (api, browser): (Vec<_>, Vec<_>) =
        sessions.iter().partition(|s| format!("{:?}", s.kind) == "API");
    let shell = Shell::new(app, Some(user), Section::Tokens);

    let revoke = |id: &str, label: &str| {
        html! {
            form.inline method="post" action={"/tokens/" (id) "/revoke"} {
                (csrf_input(&csrf))
                button.btn.sm.danger type="submit" data-confirm="Click again to revoke" { span.label { (label) } }
            }
        }
    };

    render(&shell, "Tokens", html! {
        div.head {
            div.head-text {
                h1 { "Tokens & sessions" }
                p.lede {
                    "API tokens work anywhere a password does — as " code { "Authorization: Bearer …" }
                    ", or as the Basic-Auth password for the Terraform backend."
                }
            }
        }
        @if let Some(e) = error { div.alert.coral style="margin-bottom:18px" { (e) } }
        @if let Some((name, token)) = created {
            section.card style="margin-bottom:18px;border-color:rgba(168,220,108,.35)" {
                div.card-h { h2 { (icon("check")) "Token " span.mono { (name) } " created" } }
                div.card-b {
                    div.secret {
                        span { (token) }
                        button.btn.sm type="button" data-copy=(token) { (icon("copy")) span.label { "Copy" } }
                    }
                    p.note { "Copy it now — it is never shown again." }
                }
            }
        }
        div.grid.halves {
            section.card {
                div.card-h {
                    h2 { (icon("key")) "API tokens" }
                    span.aside { (api.len()) }
                }
                div.card-b {
                    form method="post" action="/tokens" style="display:flex;gap:8px" {
                        (csrf_input(&csrf))
                        input type="text" name="name" placeholder="name, e.g. ci-deploy" required aria-label="Token name" autocomplete="off";
                        button.btn.primary type="submit" { "Create" }
                    }
                }
                @if api.is_empty() {
                    (empty("key", html! { "No API tokens yet." }))
                } @else {
                    ul.rows {
                        @for s in &api {
                            li {
                                div.grow {
                                    div.title.mono { (s.name.as_deref().unwrap_or("unnamed")) }
                                    div.sub { "created " (time(s.created)) }
                                }
                                (revoke(&s.id, "Revoke"))
                            }
                        }
                    }
                }
            }
            section.card {
                div.card-h {
                    h2 { (icon("overview")) "Browser sessions" }
                    span.aside { (browser.len()) }
                }
                @if browser.is_empty() {
                    (empty("overview", html! { "No browser sessions." }))
                } @else {
                    ul.rows {
                        @for s in &browser {
                            @let current = this.as_deref() == Some(s.id.as_str());
                            li {
                                span class={"dot " @if current { "ok" }} {}
                                div.grow {
                                    div.title { @if current { "This browser" } @else { "Signed in" } }
                                    div.sub { (time(s.created)) }
                                }
                                (revoke(&s.id, if current { "Sign out" } else { "Revoke" }))
                            }
                        }
                    }
                }
            }
        }
    })
}

#[derive(serde::Deserialize)]
pub struct CreateTokenForm {
    name: String,
    csrf: String,
}

pub async fn token_create(
    UserAuth(user): UserAuth,
    State(app): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<CreateTokenForm>,
) -> Response {
    if !app.users.verify_csrf(&f.csrf, &user.username).await {
        metrics::counter!("terrarium_auth_csrf_failures_total", "action" => "token_create").increment(1);
        let page = render_tokens(&app, &user.username, &headers, None, Some("That form expired — nothing was created. Try again.")).await;
        return (StatusCode::FORBIDDEN, page).into_response();
    }
    let name = f.name.trim();
    let session = app.users.api_key(name, &user.username).await;
    metrics::counter!("terrarium_auth_token_operations_total", "action" => "create").increment(1);
    render_tokens(&app, &user.username, &headers, Some((name, &session.token)), None).await.into_response()
}

#[derive(serde::Deserialize)]
pub struct RevokeForm {
    csrf: String,
}

pub async fn token_revoke(
    UserAuth(user): UserAuth,
    State(app): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<RevokeForm>,
) -> Response {
    if !app.users.verify_csrf(&f.csrf, &user.username).await {
        metrics::counter!("terrarium_auth_csrf_failures_total", "action" => "token_revoke").increment(1);
        let page = render_tokens(&app, &user.username, &headers, None, Some("That form expired — nothing was revoked. Try again.")).await;
        return (StatusCode::FORBIDDEN, page).into_response();
    }
    // Only allow revoking the caller's own sessions.
    let owned = app.users.list_sessions(&user.username).await.into_iter().any(|s| s.id == id);
    if owned {
        app.users.end_session(&id).await;
        metrics::counter!("terrarium_auth_token_operations_total", "action" => "revoke").increment(1);
    }
    go(&headers, "/tokens")
}
