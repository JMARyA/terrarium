//! `/diff/{name}?from=A&to=B` — version history with an inline structural
//! diff. Picking versions swaps the diff in place (htmx); without JS the
//! picker is a plain GET form.

use authur::MaybeUser;
use authur::csrf::CSRF;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::ansi;
use super::components::{delta, empty, fmt_size, icon, opt_time};
use super::data::{self, VersionInfo};
use super::layout::{Section, Shell, render};
use super::workspace::{Tab, header, missing};
use super::{require_login, valid_name};
use crate::AppState;
use crate::statediff::{Change, OutputChangeKind, StateDiff};

#[derive(serde::Deserialize)]
pub struct DiffQuery {
    from: Option<u32>,
    to: Option<u32>,
}

pub async fn page(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<DiffQuery>,
) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return missing(&app, &user.username, &name);
    }
    let versions = data::versions(&app, &name);
    if versions.is_empty() {
        return missing(&app, &user.username, &name);
    }

    // Default: the latest change.
    let latest = versions.last().map(|v| v.v).unwrap_or(1);
    let to = q.to.unwrap_or(latest);
    let from = q.from.unwrap_or_else(|| {
        versions.iter().rev().map(|v| v.v).find(|v| *v < to).unwrap_or(to)
    });
    // Always diff older → newer, whichever way round they were picked.
    let (from, to) = (from.min(to), from.max(to));

    let csrf = app.users.get_csrf(&user.username).await;
    let shell = Shell::new(&app, Some(&user.username), Section::Workspace).at(&name);
    let body = html! {
        (header(&app, &name, &csrf, Tab::History))
        div #history .grid.two {
            (diff_card(&app, &name, from, to))
            (picker(&name, &versions, from, to))
        }
    };
    render(&shell, &name, body).into_response()
}

fn picker(name: &str, versions: &[VersionInfo], from: u32, to: u32) -> Markup {
    html! {
        section.card {
            div.card-h {
                h2 { (icon("history")) "Versions" }
                span.aside { "pick two to compare" }
            }
            form method="get" action={"/diff/" (name)}
                hx-get={"/diff/" (name)} hx-trigger="change" hx-target="#history" hx-select="#history"
                hx-swap="outerHTML" hx-push-url="true" {
                div.picker-scroll.table-scroll {
                    table.picker {
                        thead { tr {
                            th.pick title="compare from" { "A" }
                            th.pick title="compare to" { "B" }
                            th { "version" }
                            th.num { "size" }
                            th.num { "Δ" }
                            th { "by" }
                            th { "pushed" }
                        } }
                        tbody {
                            @for (i, v) in versions.iter().enumerate().rev() {
                                @let prev = i.checked_sub(1).map(|p| versions[p].size);
                                tr class=[(v.v > from && v.v <= to).then_some("in-range")] {
                                    td.pick { input type="radio" name="from" value=(v.v) checked[v.v == from] aria-label={"compare from v" (v.v)}; }
                                    td.pick { input type="radio" name="to" value=(v.v) checked[v.v == to] aria-label={"compare to v" (v.v)}; }
                                    td { a.vlabel href={"/graph/" (name) "?version=" (v.v)} { "v" (v.v) } }
                                    td.num { (fmt_size(v.size)) }
                                    td.num { (delta(v.size, prev)) }
                                    td.small {
                                        (v.user.as_deref().unwrap_or("—"))
                                        @if let Some(from) = v.restored_from { span.faint { " ↺v" (from) } }
                                    }
                                    td.small.muted { (opt_time(v.at)) }
                                }
                            }
                        }
                    }
                }
                noscript { div.card-b { button.btn.sm type="submit" { "Compare" } } }
            }
        }
    }
}

fn diff_card(app: &AppState, name: &str, from: u32, to: u32) -> Markup {
    let heading = html! {
        div.card-h {
            h2 { (icon("history")) span.mono { "v" (from) } (icon("arrow")) span.mono { "v" (to) } }
        }
    };
    if from == to {
        return html! { section.card { (heading) (empty("history", html! { "Pick two different versions." })) } };
    }
    let a = app.state.get_version(name, from).ok().flatten();
    let b = app.state.get_version(name, to).ok().flatten();
    let (Some(a), Some(b)) = (a, b) else {
        return html! { section.card { (heading) (empty("alert", html! { "One of these versions doesn't exist." })) } };
    };

    let diff = crate::statediff::diff_states(&a, &b);
    html! {
        section.card {
            (heading)
            @match &diff {
                _ if diff.is_empty() => (empty("check", html! { "Identical — nothing changed between these versions." })),
                StateDiff::Structured { terraform_version, serial, changes, outputs } => {
                    @let added = changes.iter().filter(|c| matches!(c, Change::Added(_))).count();
                    @let removed = changes.iter().filter(|c| matches!(c, Change::Removed(_))).count();
                    @let modified = changes.len() - added - removed;
                    div.card-b.diff-summary {
                        @if added > 0 { span.chip.moss { "+" (added) " added" } }
                        @if removed > 0 { span.chip.coral { "−" (removed) " removed" } }
                        @if modified > 0 { span.chip.amber { "~" (modified) " changed" } }
                        @if changes.is_empty() { span.chip.ghost { "no resource changes" } }
                        @if !outputs.is_empty() { span.chip.dew { (outputs.len()) " output" (super::data::plural(outputs.len())) } }
                        @if let Some((a, b)) = serial { span.chip.mono.ghost { "serial " (a) " → " (b) } }
                        @if let Some((a, b)) = terraform_version { span.chip.mono.ghost { "tf " (a) " → " (b) } }
                    }
                    div style="border-top:1px solid var(--line)" {
                        @for (i, change) in changes.iter().enumerate() {
                            @match change {
                                Change::Added(addr) => div.change.add { div.row { span.sig { "+" } span.addr { (addr) } } },
                                Change::Removed(addr) => div.change.del { div.row { span.sig { "−" } span.addr { (addr) } } },
                                Change::Modified { addr, instances } => details.change.mod open[i < 12] {
                                    summary { span.sig { "~" } span.addr { (addr) } }
                                    pre {
                                        @for inst in instances {
                                            @if inst.multi { span.inst { "[" (inst.index) "]\n" } }
                                            (ansi::to_html(&inst.diff)) "\n"
                                        }
                                    }
                                },
                            }
                        }
                    }
                    @if !outputs.is_empty() {
                        div.card-b style="border-top:1px solid var(--line);padding-bottom:6px" { h3.muted style="margin:0" { "Outputs" } }
                        div {
                            @for o in outputs {
                                @let (class, sig) = match o.kind { OutputChangeKind::Added => ("add", "+"), OutputChangeKind::Removed => ("del", "−"), OutputChangeKind::Modified { .. } => ("mod", "~") };
                                @match &o.kind {
                                    OutputChangeKind::Modified { diff: Some(d) } => details.change class=(class) open {
                                        summary { span.sig { (sig) } span.addr { "output." (o.name) } }
                                        pre { (ansi::to_html(d)) }
                                    },
                                    _ => div.change class=(class) {
                                        div.row {
                                            span.sig { (sig) } span.addr { "output." (o.name) }
                                            @if o.sensitive { span.chip.amber style="margin-left:auto" { (icon("lock")) "sensitive · value hidden" } }
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
                StateDiff::Raw(out) => {
                    div.card-b { div.alert.amber { "Not Terraform state — showing a plain JSON diff." } }
                    pre.code { (ansi::to_html(out)) }
                }
                StateDiff::Error(e) => div.card-b { div.alert.coral { (e) } },
            }
        }
    }
}
