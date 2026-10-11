//! `/graph/{name}?version=N` — what's inside a state version.

use std::collections::BTreeMap;

use authur::MaybeUser;
use authur::csrf::CSRF;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::components::{empty, fmt_size, icon};
use super::layout::{Section, Shell, render};
use super::workspace::{Tab, header, missing};
use super::{graph, require_login, valid_name};
use crate::AppState;
use crate::tfstate::TfState;

#[derive(serde::Deserialize)]
pub struct VersionQuery {
    version: Option<u32>,
}

pub async fn page(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return missing(&app, &user.username, &name);
    }

    let versions = app.state.list_versions(&name);
    let current = versions.last().copied();
    let selected = q.version.or(current);
    let data = match selected {
        Some(v) => app.state.get_version(&name, v),
        None => app.state.get(&name),
    };
    let Some(data) = data.ok().flatten() else {
        return missing(&app, &user.username, &name);
    };

    let csrf = app.users.get_csrf(&user.username).await;
    let shell = Shell::new(&app, Some(&user.username), Section::Workspace).at(&name);
    let parsed = facet_json::from_str::<TfState>(&String::from_utf8_lossy(&data));

    let body = html! {
        (header(&app, &name, &csrf, Tab::State))
        div.head-meta style="margin:-6px 0 18px" {
            label.muted.small for="version" { "Version" }
            select #version name="version" hx-get={"/graph/" (name)} hx-trigger="change"
                hx-target="body" hx-push-url="true" {
                @for v in versions.iter().rev() {
                    option value=(v) selected[Some(*v) == selected] {
                        "v" (v) @if Some(*v) == current { "  (current)" }
                    }
                }
            }
            @if selected != current {
                @if let (Some(s), Some(c)) = (selected, current) {
                    span.chip.amber { "viewing an older version" }
                    a.btn.sm href={"/diff/" (name) "?from=" (s) "&to=" (c)} { (icon("history")) "Compare with current" }
                    button.btn.sm.quiet type="button" data-copy={"terra remote state restore " (name) " --version " (s)}
                        title="Copy the command that makes this version current again" {
                        (icon("copy")) span.label { "Copy restore command" }
                    }
                }
            }
            span.faint.small { (fmt_size(data.len() as u64)) }
        }
        @match parsed {
            Ok(state) => (overview(&state)),
            Err(_) => section.card {
                (empty("file", html! { "This version isn't Terraform state v4, so there's nothing to break down. "
                    "Raw JSON: " code { "terra remote state get " (name) } }))
            },
        }
    };
    render(&shell, &name, body).into_response()
}

fn overview(state: &TfState) -> Markup {
    // type → [(label, instances)], split by mode.
    let mut groups: BTreeMap<(bool, &str), Vec<(String, usize)>> = BTreeMap::new();
    let mut instances = 0usize;
    for r in &state.resources {
        instances += r.instances.len();
        let label = match &r.module {
            Some(m) => format!("{m}.{}", r.name),
            None => r.name.clone(),
        };
        groups.entry((r.mode == "data", &r.type_)).or_default().push((label, r.instances.len()));
    }
    let mut outputs: Vec<_> = state.outputs.iter().collect();
    outputs.sort_by(|a, b| a.0.cmp(b.0));

    html! {
        div.grid.two {
            section.card {
                div.card-h {
                    h2 { (icon("layers")) "Resources" }
                    span.aside { (state.resources.len()) " · " (instances) " instances" }
                }
                @if groups.is_empty() {
                    (empty("layers", html! { "No resources in this state." }))
                } @else {
                    div.card-b style="padding-bottom:6px" {
                        input.filter type="text" placeholder="Filter by type or name…" data-filter="#resources"
                            aria-label="Filter resources" autocomplete="off" spellcheck="false";
                    }
                    div #resources {
                        @for ((is_data, ty), names) in &groups {
                            details.rgroup data-group=(ty) open[state.resources.len() <= 60] {
                                summary {
                                    (icon("chevron"))
                                    span class={"kind " (if *is_data { "data" } else { "managed" })} { (if *is_data { "data" } else { "res" }) }
                                    span { (ty) }
                                    span.n { (names.len()) }
                                }
                                ul {
                                    @for (label, n) in names {
                                        li { span { (label) } @if *n != 1 { span.k { "×" (n) } } }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div.stack {
                section.card {
                    div.card-h { h2 { (icon("file")) "Specimen" } }
                    div.card-b {
                        dl.kv {
                            dt { "Terraform" } dd { (state.terraform_version) }
                            dt { "Serial" } dd { (state.serial) }
                            dt { "Lineage" } dd { (state.lineage) }
                            dt { "Format" } dd { "v" (state.version) }
                        }
                    }
                }
                section.card {
                    div.card-h {
                        h2 { (icon("arrow")) "Outputs" }
                        span.aside { (outputs.len()) }
                    }
                    @if outputs.is_empty() {
                        (empty("arrow", html! { "No outputs." }))
                    } @else {
                        ul.rows {
                            @for (k, out) in &outputs {
                                li {
                                    span.grow.title.mono { (k) }
                                    @if out.sensitive { span.chip.amber { (icon("lock")) "sensitive" } }
                                }
                            }
                        }
                    }
                }
            }
        }
        @if let Some(svg) = graph::render(state) {
            section.card style="margin-top:18px" {
                div.card-h {
                    h2 { (icon("graph")) "Dependencies" }
                    div.aside.legend {
                        span { span.swatch.managed {} "managed" }
                        span { span.swatch.data {} "data source" }
                        span { "hover to trace · click to pin" }
                    }
                }
                div.graph-wrap { (svg) }
            }
        }
    }
}
