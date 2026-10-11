//! `/` — the fleet at a glance.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use authur::MaybeUser;
use chrono::Duration;
use maud::{Markup, html};

use super::components::{codeblock, delta, empty, icon, op_label, spark, time};
use super::data::{self, Event, EventKind, plural};
use super::layout::{Section, Shell, render};
use super::{origin, require_login};
use crate::AppState;

pub async fn page(maybe: MaybeUser, State(app): State<AppState>, headers: HeaderMap) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    let shell = Shell::new(&app, Some(&user.username), Section::Overview);

    let names = app.state.list(None, false);
    let archived = app.state.list(None, true);
    let locks = app.locks.list();
    let reports = app.violations.list();
    let issues = reports.iter().filter(|r| r.error.is_some() || r.deny_count() > 0).count();
    let pushes_24h = data::pushes_since(&app, &names, Duration::hours(24));
    let per_day = data::pushes_per_day(&app, &names, 14);
    let namespaces = {
        let mut ns: Vec<&str> = names.iter().filter_map(|n| n.rsplit_once('/').map(|(p, _)| p)).collect();
        ns.sort_unstable();
        ns.dedup();
        ns.len()
    };

    let body = html! {
        div.head {
            div.head-text {
                div.crumbs { span { (origin(&headers).trim_start_matches("https://").trim_start_matches("http://")) } }
                h1 { "Overview" }
                p.lede {
                    (names.len()) " workspace" (plural(names.len()))
                    @if namespaces > 0 { " across " (namespaces) " namespace" (plural(namespaces)) }
                    @if !archived.is_empty() { ", " (archived.len()) " archived" }
                    "."
                }
            }
        }

        div.vitals {
            div.vital {
                div.n { (names.len()) }
                div.l { "workspaces" }
            }
            div class={"vital" @if !locks.is_empty() { " amber" }} {
                div.n { (locks.len()) }
                div.l { "locked right now" }
            }
            div class={"vital" @if issues > 0 { " coral" }} {
                div.n { (issues) }
                div.l { "with policy issues" }
            }
            div class={"vital" @if pushes_24h > 0 { " moss" }} {
                div.n { (pushes_24h) }
                div.l { "pushes in 24h" }
                (spark(&per_day))
            }
        }

        @if names.is_empty() && archived.is_empty() {
            (first_steps(&headers))
        } @else {
            div.grid.two {
                section.card {
                    div.card-h {
                        h2 { (icon("history")) "Activity" }
                        span.aside { "pushes & locks, newest first" }
                    }
                    (timeline(&data::all_events(&app, &names, 20), true))
                }
                div.stack {
                    (locks_card(&app))
                    (attention(&app, &names))
                    (quiet(&app, &names))
                }
            }
        }
    };
    render(&shell, "Overview", body).into_response()
}

/// `GET /ui/locks` — the live locks card, polled by the overview.
pub async fn locks_fragment(maybe: MaybeUser, State(app): State<AppState>) -> Response {
    if maybe.user().is_none() {
        return require_login();
    }
    locks_card(&app).into_response()
}

fn locks_card(app: &AppState) -> Markup {
    let mut locks: Vec<_> = app.locks.list().into_iter().collect();
    locks.sort_by(|a, b| a.0.cmp(&b.0));
    html! {
        section.card #locks-card .glow-amber[!locks.is_empty()]
            hx-get="/ui/locks" hx-trigger="every 5s [document.visibilityState === 'visible']" hx-swap="outerHTML" {
            div.card-h {
                h2 { (icon("lock")) "Held locks" }
                span.aside { span class={"dot " @if locks.is_empty() { "ok" } @else { "locked" }} {} "live" }
            }
            @if locks.is_empty() {
                (empty("unlock", html! { "Nothing is locked. All workspaces are free." }))
            } @else {
                ul.rows {
                    @for (name, info) in &locks {
                        @let expired = app.locks.is_expired(name);
                        li {
                            span class={"dot " @if expired { "expired" } @else { "locked" }} {}
                            a.grow.row-link href={"/w/" (name)} {
                                div.title.mono { (name) }
                                div.sub {
                                    (info.Who.as_deref().unwrap_or("unknown"))
                                    @if let Some(op) = &info.Operation { " · " (op_label(op)) }
                                }
                            }
                            @if expired { span.chip.coral { "expired" } }
                            span.small.faint {
                                @if let Some(at) = data::parse_time(info.Created.as_deref()) { (time(at)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn attention(app: &AppState, names: &[String]) -> Markup {
    struct Item<'a> {
        name: &'a str,
        chip: Markup,
    }
    let mut items: Vec<Item> = Vec::new();
    for name in names {
        if app.locks.get(name).is_some() && app.locks.is_expired(name) {
            items.push(Item { name, chip: html! { span.chip.coral { (icon("lock")) "lock expired" } } });
        }
        if let Some(r) = app.violations.get(name) {
            if r.error.is_some() {
                items.push(Item { name, chip: html! { span.chip.amber { (icon("alert")) "not checked" } } });
            } else if r.deny_count() > 0 {
                let n = r.deny_count();
                items.push(Item { name, chip: html! { span.chip.coral { (icon("policy")) (n) " violation" (plural(n)) } } });
            } else if r.warn_count() > 0 {
                let n = r.warn_count();
                items.push(Item { name, chip: html! { span.chip.amber { (n) " warning" (plural(n)) } } });
            }
        }
    }
    html! {
        section.card .glow-coral[!items.is_empty()] {
            div.card-h {
                h2 { (icon("alert")) "Needs attention" }
                span.aside { (items.len()) }
            }
            @if items.is_empty() {
                @if app.policies.list().is_empty() {
                    (empty("policy", html! { "No expired locks. No policies yet — " a.link href="/policies" { "add some" } "." }))
                } @else {
                    (empty("check", html! { "All clear. Every workspace passes policy." }))
                }
            } @else {
                ul.rows {
                    @for item in &items {
                        li {
                            a.grow.row-link href={"/w/" (item.name)} { div.title.mono { (item.name) } }
                            (item.chip)
                        }
                    }
                }
            }
        }
    }
}

/// Workspaces nobody has pushed to in a long time.
fn quiet(app: &AppState, names: &[String]) -> Markup {
    let mut quiet: Vec<(&String, i64)> = names.iter().filter_map(|n| Some((n, data::quiet_days(app, n)?))).collect();
    if quiet.is_empty() {
        return html! {};
    }
    quiet.sort_by(|a, b| b.1.cmp(&a.1));
    html! {
        section.card {
            div.card-h {
                h2 { (icon("sprout")) "Gone quiet" }
                span.aside { "no push in " (data::QUIET_AFTER_DAYS) "+ days" }
            }
            ul.rows {
                @for (name, days) in &quiet {
                    li {
                        span.dot.quiet {}
                        a.grow.row-link href={"/w/" (name)} { div.title.mono { (name) } }
                        span.small.faint { (days) "d" }
                    }
                }
            }
            p.note style="padding:0 16px 14px;margin-top:0" { "Retired? Archive them to make that explicit and keep them out of the way." }
        }
    }
}

/// Activity timeline. `show_ws` names the workspace on each entry, for the
/// fleet-wide feed.
pub(super) fn timeline(events: &[Event], show_ws: bool) -> Markup {
    if events.is_empty() {
        return empty("history", html! { "No activity recorded yet." });
    }
    html! {
        ol.timeline {
            @for e in events {
                li {
                    @match &e.kind {
                        EventKind::Push { version, size, prev, user, restored_from } => {
                            span.node.push { (icon(if restored_from.is_some() { "history" } else { "push" })) }
                            div.what {
                                @if let Some(u) = user { b { (u) } " " }
                                @match restored_from {
                                    Some(from) => {
                                        "restored "
                                        a href={"/graph/" (e.workspace) "?version=" (from)} { b.mono { "v" (from) } }
                                        " as "
                                        a href={"/graph/" (e.workspace) "?version=" (version)} { b.mono { "v" (version) } }
                                    }
                                    None => {
                                        "pushed "
                                        a href={"/graph/" (e.workspace) "?version=" (version)} { b.mono { "v" (version) } }
                                    }
                                }
                                @if show_ws { " to " a.ws href={"/w/" (e.workspace)} { (e.workspace) } }
                                " " (delta(*size, *prev))
                            }
                        }
                        EventKind::Lock { who, operation } => {
                            span.node.lock { (icon("lock")) }
                            div.what {
                                b { (who) } " locked"
                                @if show_ws { " " a.ws href={"/w/" (e.workspace)} { (e.workspace) } }
                                @if let Some(op) = operation {
                                    " for " b { (op_label(op)) }
                                }
                            }
                        }
                    }
                    span.meta { (time(e.at)) }
                }
            }
        }
    }
}

fn first_steps(headers: &HeaderMap) -> Markup {
    let origin = origin(headers);
    let backend = format!(
        r#"terraform {{
  backend "http" {{
    address        = "{origin}/state/infra/prod"
    lock_address   = "{origin}/lock/infra/prod"
    unlock_address = "{origin}/lock/infra/prod"
  }}
}}"#
    );
    html! {
        section.card {
            div.card-h { h2 { (icon("sprout")) "Plant your first workspace" } }
            div.card-b {
                p.muted style="margin-top:0" {
                    "Point a Terraform or OpenTofu backend at this server. The workspace appears here on the first push — "
                    "names can nest, like " code { "infra/prod" } "."
                }
                (codeblock(&backend))
                p.note {
                    "Authenticate with " code { "TF_HTTP_USERNAME" } " / " code { "TF_HTTP_PASSWORD" }
                    " — an " a.link href="/tokens" { "API token" } " works as the password. More in " a.link href="/help" { "Help" } "."
                }
            }
        }
    }
}
