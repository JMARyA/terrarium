//! `/policies` — what's loaded, how it's enforced, and what it found.

use authur::MaybeUser;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use maud::html;

use super::components::{empty, icon, time};
use super::data::{parse_time, plural};
use super::layout::{Section, Shell, render};
use super::require_login;
use crate::AppState;
use crate::policy::{Origin, Site};

pub async fn page(State(app): State<AppState>, maybe: MaybeUser) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    let shell = Shell::new(&app, Some(&user.username), Section::Policies);
    let policies = app.policies.list();
    let config = app.policies.config();
    let reports = app.violations.list();

    let body = html! {
        div.head {
            div.head-text {
                h1 { "Policies" }
                p.lede {
                    "Rego rules checked before " code { "terra apply" } " and against every pushed state. "
                    "Author them in " code { ".terrarium/policies/" } ", try them with " code { "terra policy test" }
                    ", share them with " code { "terra policy push" } "."
                }
            }
        }
        div.grid.two {
            div.stack {
                section.card {
                    div.card-h {
                        h2 { (icon("policy")) "Loaded" }
                        span.aside { (policies.len()) }
                    }
                    @if policies.is_empty() {
                        (empty("policy", html! {
                            "No policies yet. Push some with " code { "terra policy push" }
                            ", or drop " code { ".rego" } " files into the server's " code { "policies/" } " directory."
                        }))
                    } @else {
                        ul.rows {
                            @for p in &policies {
                                li style="align-items:flex-start" {
                                    span class={"dot " @if p.enabled { "ok" } @else { "archived" }} style="margin-top:7px" {}
                                    div.grow {
                                        div.title.mono { (p.name) }
                                        div.sub {
                                            @if let Some(at) = parse_time(Some(&p.updated)) { "updated " (time(at)) } @else { (p.updated) }
                                            " by " (p.updated_by)
                                        }
                                    }
                                    div.head-meta style="margin:0;justify-content:flex-end" {
                                        @if !p.enabled { span.chip.ghost { "disabled" } }
                                        span.chip.mono { @if p.workspace.is_empty() { "global" } @else { (p.workspace) } }
                                        @for s in &p.sites {
                                            span.chip.dew { (match s { Site::State => "state", Site::Plan => "plan" }) }
                                        }
                                        // Origin decides whether the API may change a policy —
                                        // "why can't I delete this" should answer itself.
                                        @match p.origin {
                                            Origin::File => span.chip.ghost title="loaded from the server's policies/ directory; read-only over the API" { "file" },
                                            Origin::Api => span.chip.ghost { "api" },
                                            Origin::Local => span.chip.ghost { "local" },
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                section.card {
                    div.card-h {
                        h2 { (icon("alert")) "Findings" }
                        span.aside { (reports.len()) " workspace" (plural(reports.len())) }
                    }
                    @if reports.is_empty() {
                        (empty("check", html! { @if policies.is_empty() { "Nothing to report." } @else { "Every workspace passes." } }))
                    } @else {
                        div.table-scroll { table {
                            thead { tr { th { "workspace" } th.num { "deny" } th.num { "warn" } th { "pushed by" } th { "checked" } } }
                            tbody {
                                @for r in &reports {
                                    tr {
                                        td.mono { a href={"/w/" (r.workspace)} { (r.workspace) } }
                                        td.num {
                                            @if r.error.is_some() { span.chip.amber { "not checked" } }
                                            @else if r.deny_count() > 0 { span style="color:var(--coral)" { (r.deny_count()) } }
                                            @else { span.faint { "0" } }
                                        }
                                        td.num { @if r.warn_count() > 0 { span style="color:var(--amber)" { (r.warn_count()) } } @else { span.faint { "0" } } }
                                        td.muted { (r.user) }
                                        td.muted.small { @if let Some(at) = parse_time(Some(&r.checked)) { (time(at)) } }
                                    }
                                }
                            }
                        } }
                    }
                }
            }
            div.stack {
                section.card {
                    div.card-h { h2 { (icon("layers")) "Enforcement" } }
                    @if config.is_empty() {
                        div.card-b.muted {
                            "Defaults apply everywhere: a " code { "deny" } " blocks the apply, and pushed state is linted."
                        }
                    } @else {
                        table {
                            thead { tr { th { "scope" } th { "mode" } th { "lint" } } }
                            tbody {
                                @for c in &config {
                                    tr {
                                        td.mono { @if c.scope.is_empty() { "global" } @else { (c.scope) } }
                                        td.mono { (c.mode.as_str()) }
                                        td { @if c.lint { span.chip.moss { "on" } } @else { span.chip.ghost { "off" } } }
                                    }
                                }
                            }
                        }
                    }
                }
                div.alert.amber {
                    b { "A guardrail, not access control. " }
                    "Anyone who can push state can skip the apply-time check with " code { "--policy=off" }
                    " or by running " code { "tofu" } " directly. The server-side lint runs regardless — that's what Findings shows."
                }
            }
        }
    };
    render(&shell, "Policies", body).into_response()
}
