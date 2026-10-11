//! `/w/{name}` — one workspace: its lifeline, lock, policy status and
//! activity.

use authur::MaybeUser;
use authur::csrf::CSRF;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use maud::{Markup, html};

use super::components::{crumbs, csrf_input, empty, fmt_size, icon, op_label, opt_time, time};
use super::data::{self, Health, VersionInfo, plural};
use super::layout::{Section, Shell, render};
use super::overview::timeline;
use super::{not_found, require_login, valid_name};
use crate::AppState;
use crate::tfstate::TfState;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Tab {
    Overview,
    State,
    History,
}

/// Shared header for the three workspace tabs.
pub fn header(app: &AppState, name: &str, csrf: &str, tab: Tab) -> Markup {
    let archived = app.state.is_archived(name);
    let health = Health::of(app, name, archived);
    let versions = app.state.list_versions(name);
    let current = versions.last().copied();
    let state = app
        .state
        .get(name)
        .ok()
        .flatten()
        .and_then(|b| facet_json::from_str::<TfState>(&String::from_utf8_lossy(&b)).ok());
    let leaf = name.rsplit('/').next().unwrap_or(name);
    let tab_link = |t: Tab, href: String, ic: &str, label: &str| {
        html! { a href=(href) aria-current=[(tab == t).then_some("page")] { (icon(ic)) (label) } }
    };

    html! {
        div.head {
            div.head-text {
                (crumbs(name))
                h1 { span.mono { (leaf) } }
                div.head-meta {
                    @match health {
                        Health::Ok => span.chip.moss { span.dot.ok {} "idle" },
                        Health::Locked => span.chip.amber { span.dot.locked {} "locked" },
                        Health::ExpiredLock => span.chip.coral { span.dot.expired {} "lock expired" },
                        Health::Archived => span.chip.ghost { (icon("archive")) "archived · read-only" },
                        Health::Deny(_) => span.chip.coral { (icon("policy")) (health.label()) },
                        Health::Warn(_) | Health::Unchecked => span.chip.amber { (icon("policy")) (health.label()) },
                    }
                    @if let Some(v) = current { span.chip.mono { "v" (v) } }
                    @if let Some(s) = &state {
                        span.chip.mono title="serial" { "serial " (s.serial) }
                        span.chip.mono { (s.resources.len()) " resource" (plural(s.resources.len())) }
                        span.chip.mono.ghost { "tf " (s.terraform_version) }
                    }
                }
            }
            div.head-actions {
                @if archived {
                    form.inline method="post" action={"/ui/unarchive/" (name)} {
                        (csrf_input(csrf))
                        button.btn type="submit" data-confirm="Make writable again?" {
                            (icon("archive")) span.label { "Unarchive" }
                        }
                    }
                } @else if current.is_some() {
                    form.inline method="post" action={"/ui/archive/" (name)} {
                        (csrf_input(csrf))
                        button.btn.danger type="submit" data-confirm="Archive — reject all pushes?" {
                            (icon("archive")) span.label { "Archive" }
                        }
                    }
                }
            }
        }
        nav.tabs {
            (tab_link(Tab::Overview, format!("/w/{name}"), "overview", "Overview"))
            (tab_link(Tab::State, format!("/graph/{name}"), "layers", "State"))
            (tab_link(Tab::History, format!("/diff/{name}"), "history", "History"))
        }
    }
}

pub async fn page_html(app: &AppState, user: &str, name: &str) -> Response {
    let shell = Shell::new(app, Some(user), Section::Workspace).at(name);
    let versions = data::versions(app, name);
    if versions.is_empty() && !matches!(app.state.get(name), Ok(Some(_))) {
        return missing(app, user, name);
    }
    let csrf = app.users.get_csrf(user).await;
    let hooks = app.webhooks.list_for(name).await;

    let body = html! {
        (header(app, name, &csrf, Tab::Overview))
        div.grid.two {
            div.stack {
                (lifeline(name, &versions))
                section.card {
                    div.card-h { h2 { (icon("history")) "Activity" } }
                    (timeline(&data::events_for(app, name, 25), false))
                }
            }
            div.stack {
                (lock_card(app, name, &csrf))
                (policy_card(app, name))
                section.card {
                    div.card-h {
                        h2 { (icon("webhook")) "Webhooks" }
                        span.aside { (hooks.len()) }
                    }
                    @if hooks.is_empty() {
                        (empty("webhook", html! { "None. Add one with " code { "terra remote webhook add" } "." }))
                    } @else {
                        ul.rows {
                            @for h in &hooks {
                                li {
                                    div.grow {
                                        div.title.mono { (h.url) }
                                        div.sub {
                                            @if h.events.is_empty() { "all events" } @else { (h.events.join(", ")) }
                                            " · " span.mono { (h.id) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    render(&shell, name, body).into_response()
}

pub async fn page(maybe: MaybeUser, State(app): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return missing(&app, &user.username, &name);
    }
    page_html(&app, &user.username, &name).await
}

pub fn missing(app: &AppState, user: &str, name: &str) -> Response {
    let shell = Shell::new(app, Some(user), Section::Workspace);
    not_found(render(&shell, "Not found", html! {
        div.head { div.head-text { div.crumbs { a href="/" { "workspaces" } } h1 { span.mono { (name) } } } }
        section.card { (empty("folder", html! { "No such workspace. It appears on its first push." })) }
    }))
}

/// A bar per version, height by state size. Click a bar to open that version.
fn lifeline(name: &str, versions: &[VersionInfo]) -> Markup {
    const SHOWN: usize = 64;
    let shown = &versions[versions.len().saturating_sub(SHOWN)..];
    let max = shown.iter().map(|v| v.size).max().unwrap_or(1).max(1) as f64;
    let current = versions.last().map(|v| v.v);
    html! {
        section.card {
            div.card-h {
                h2 { (icon("graph")) "Lifeline" }
                span.aside {
                    (versions.len()) " version" (plural(versions.len()))
                    @if versions.len() > SHOWN { " · last " (SHOWN) " shown" }
                }
            }
            @if shown.is_empty() {
                (empty("history", html! { "No versions recorded." }))
            } @else {
                div.lifeline {
                    div.lifeline-bars {
                        @for v in shown {
                            @let h = 8.0 + (v.size as f64 / max) * 92.0;
                            a href={"/graph/" (name) "?version=" (v.v)}
                              class=[(Some(v.v) == current).then_some("current")]
                              style={"height:" (format!("{h:.0}")) "%"}
                              title={"v" (v.v) " · " (fmt_size(v.size)) @if let Some(u) = &v.user { " · " (u) } @if let Some(at) = v.at { " · " (at.format("%Y-%m-%d %H:%M UTC")) }} {}
                        }
                    }
                    div.lifeline-axis {
                        span { "v" (shown[0].v) " · " (opt_time(shown[0].at)) }
                        @if let Some(last) = shown.last() {
                            span { "v" (last.v) " · " (fmt_size(last.size)) " · " (opt_time(last.at)) }
                        }
                    }
                }
            }
        }
    }
}

/// `GET /ui/lock/{name}` — the live lock card, polled by the workspace page.
pub async fn lock_fragment(maybe: MaybeUser, State(app): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }
    let csrf = app.users.get_csrf(&user.username).await;
    lock_card(&app, &name, &csrf).into_response()
}

pub fn lock_card(app: &AppState, name: &str, csrf: &str) -> Markup {
    let lock = app.locks.get(name);
    let expired = lock.is_some() && app.locks.is_expired(name);
    let since = lock.as_ref().and_then(|l| data::parse_time(l.Created.as_deref()));
    let ttl = app.locks.ttl();
    // Progress toward expiry, from the client's `Created`. Approximate — the
    // server measures from its own acquisition time — but close enough to
    // show how long a lock has been sitting there.
    let progress = match (since, ttl) {
        (Some(at), Some(ttl)) if !ttl.is_zero() => {
            Some(((Utc::now() - at).num_seconds().max(0) as f64 / ttl.as_secs() as f64).min(1.0))
        }
        _ => None,
    };
    let state_class = match (&lock, expired) {
        (None, _) => "lockstate",
        (Some(_), false) => "lockstate held",
        (Some(_), true) => "lockstate expired",
    };
    html! {
        section.card #lock-card .glow-amber[lock.is_some()]
            hx-get={"/ui/lock/" (name)} hx-trigger="every 5s [document.visibilityState === 'visible']"
            hx-swap="outerHTML" {
            div.card-h {
                h2 { (icon("lock")) "Lock" }
                span.aside { span class={"dot " @if lock.is_some() { "locked" } @else { "ok" }} {} "live" }
            }
            div.card-b {
                div class=(state_class) {
                    span.glyph { (icon(if lock.is_some() { "lock" } else { "unlock" })) }
                    div.body {
                        @match &lock {
                            None => {
                                div.state { "Unlocked" }
                                div.detail { "Free for the next plan or apply." }
                            }
                            Some(info) => {
                                div.state {
                                    @if expired { "Lock expired" } @else { "Locked" }
                                    @if let Some(op) = &info.Operation { span.muted { " · " (op_label(op)) } }
                                }
                                div.detail {
                                    "by " b { (info.Who.as_deref().unwrap_or("unknown")) }
                                    @if let Some(at) = since { ", " (time(at)) }
                                }
                                div.detail.mono.small { "ID " (info.ID) }
                                @if let Some(p) = progress {
                                    div.ttl title="time until the lock may be taken over" {
                                        span style={"width:" (format!("{:.0}", p * 100.0)) "%"} {}
                                    }
                                    div.detail.small {
                                        @if expired { "Past its TTL — the next lock request takes it over." }
                                        @else if let (Some(at), Some(ttl)) = (since, ttl) {
                                            @let left = (at + chrono::Duration::from_std(ttl).unwrap_or_default() - Utc::now()).num_minutes().max(0);
                                            "Expires in " (left / 60) "h " (left % 60) "m unless released."
                                        }
                                    }
                                }
                                div.actions {
                                    form.inline method="post" action={"/ui/unlock/" (name)}
                                        hx-post={"/ui/unlock/" (name)} hx-target="#lock-card" hx-swap="outerHTML" {
                                        (csrf_input(csrf))
                                        button.btn.sm.danger.arm-amber type="submit"
                                            data-confirm="Click again to force-unlock" {
                                            (icon("unlock")) span.label { "Force unlock" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn policy_card(app: &AppState, name: &str) -> Markup {
    let report = app.violations.get(name);
    if report.is_none() && app.policies.list().is_empty() {
        return html! {};
    }
    html! {
        section.card .glow-coral[report.as_ref().is_some_and(|r| r.deny_count() > 0)] {
            div.card-h {
                h2 { (icon("policy")) "Policy" }
                @if let Some(r) = &report { span.aside { "checked " @if let Some(at) = data::parse_time(Some(&r.checked)) { (time(at)) } } }
            }
            @match &report {
                None => (empty("check", html! { "Passes every policy." })),
                Some(r) => {
                    @if let Some(err) = &r.error {
                        div.card-b { div.alert.amber { b { "Could not be checked. " } (err) } }
                    }
                    @if !r.violations.is_empty() {
                        ul.rows {
                            @for v in &r.violations {
                                li style="align-items:flex-start" {
                                    @match v.severity {
                                        crate::policy::Severity::Deny => span.chip.coral { "deny" },
                                        crate::policy::Severity::Warn => span.chip.amber { "warn" },
                                    }
                                    div.grow {
                                        div style="white-space:normal" { (v.message) }
                                        div.sub.mono { (v.policy) }
                                    }
                                }
                            }
                        }
                    }
                    p.note style="padding:0 16px 14px" {
                        "State pushed by " b { (r.user) }
                        @if let Some(v) = r.version { " as " span.mono { "v" (v) } }
                        "."
                    }
                }
            }
        }
    }
}
