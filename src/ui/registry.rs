//! `/registry/…` — the provider registry and its rendered docs. Public pages:
//! anyone who can reach the registry API can read these.

use authur::MaybeUser;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, PreEscaped, html};

use super::components::{codeblock, empty, icon};
use super::layout::{Section, Shell, render};
use super::{not_found, origin};
use crate::AppState;
use crate::pathguard::is_safe_segment;

pub async fn index(State(app): State<AppState>, maybe: MaybeUser) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    let shell = Shell::new(&app, user.as_deref(), Section::Registry);
    let providers = app.registry.list_providers();
    let mirror = app.mirror_status.read().await;

    let body = html! {
        div.head {
            div.head-text {
                h1 { "Provider registry" }
                p.lede {
                    "Providers published here, plus anything cached by the network mirror. "
                    "Source them as " code { "<this host>/<namespace>/<type>" } "."
                }
            }
            div.head-meta {
                @if mirror.running { span.chip.dew { span.dot.locked {} "mirror syncing" } }
                @else if let Some(t) = mirror.last_sync_finished {
                    @if let Some(at) = chrono::DateTime::from_timestamp(t as i64, 0) {
                        span.chip.ghost { "mirror synced " (super::components::time(at)) }
                    }
                }
                @if mirror.total_errors > 0 { span.chip.coral { (mirror.total_errors) " mirror errors" } }
            }
        }
        section.card {
            div.card-h {
                h2 { (icon("registry")) "Providers" }
                span.aside { (providers.len()) }
            }
            @if providers.is_empty() {
                (empty("registry", html! { "No providers yet. See " a.link href="/help#registry" { "Help" } " for publishing and mirroring." }))
            } @else {
                ul.rows {
                    @for (ns, tp) in &providers {
                        @let versions = app.registry.list_versions(ns, tp);
                        @let latest = versions.last().cloned().unwrap_or_default();
                        @let plats = platforms(&app, ns, tp, &latest);
                        li {
                            span.avatar style="border-radius:9px;background:var(--surface-2);color:var(--moss);border:1px solid var(--line-2)" {
                                (tp.chars().next().unwrap_or('?'))
                            }
                            a.grow.row-link href={"/registry/" (ns) "/" (tp)} {
                                div.title.mono { span.muted { (ns) "/" } (tp) }
                                div.sub { (plats.join(" · ")) }
                            }
                            @if !latest.is_empty() && app.registry.has_docs(ns, tp, &latest) { span.chip.moss { "docs" } }
                            span.chip.mono { (latest) }
                            span.small.faint { (versions.len()) " version" (super::data::plural(versions.len())) }
                        }
                    }
                }
            }
        }
    };
    render(&shell, "Registry", body).into_response()
}

/// `GET /registry/{namespace}/{type}` — the latest version's docs.
pub async fn provider(State(app): State<AppState>, Path((ns, tp)): Path<(String, String)>, maybe: MaybeUser) -> Response {
    let versions = app.registry.list_versions(&ns, &tp);
    match versions.last() {
        Some(latest) => Redirect::to(&format!("/registry/{ns}/{tp}/{latest}/docs")).into_response(),
        None => missing(&app, maybe.user().map(|u| u.username.as_str()), &format!("{ns}/{tp}")),
    }
}

pub async fn docs_index(
    State(app): State<AppState>,
    Path((ns, tp, ver)): Path<(String, String, String)>,
    maybe: MaybeUser,
    headers: HeaderMap,
) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    docs(&app, &ns, &tp, &ver, "index", user.as_deref(), &headers)
}

pub async fn docs_page(
    State(app): State<AppState>,
    Path((ns, tp, ver, path)): Path<(String, String, String, String)>,
    maybe: MaybeUser,
    headers: HeaderMap,
) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    docs(&app, &ns, &tp, &ver, &path, user.as_deref(), &headers)
}

fn missing(app: &AppState, user: Option<&str>, what: &str) -> Response {
    let shell = Shell::new(app, user, Section::Registry);
    not_found(render(&shell, "Not found", html! {
        div.head { div.head-text { div.crumbs { a href="/registry" { "registry" } } h1 { span.mono { (what) } } } }
        section.card { (empty("registry", html! { "No such provider or version." })) }
    }))
}

fn docs(app: &AppState, ns: &str, tp: &str, ver: &str, doc_path: &str, user: Option<&str>, headers: &HeaderMap) -> Response {
    let all_versions = app.registry.list_versions(ns, tp);
    let safe = [ns, tp, ver].into_iter().chain(doc_path.split('/')).all(is_safe_segment);
    if !safe || !all_versions.iter().any(|v| v == ver) {
        return missing(app, user, &format!("{ns}/{tp}"));
    }
    let shell = Shell::new(app, user, Section::Registry);

    let categories = [
        ("Resources", "resources", app.registry.list_doc_category(ns, tp, ver, "resources")),
        ("Data sources", "data-sources", app.registry.list_doc_category(ns, tp, ver, "data-sources")),
        ("Guides", "guides", app.registry.list_doc_category(ns, tp, ver, "guides")),
    ];
    let base = format!("/registry/{ns}/{tp}/{ver}/docs");
    let content = app.registry.get_doc_file(ns, tp, ver, doc_path);
    let has_docs = categories.iter().any(|c| !c.2.is_empty()) || content.is_some();

    let main = match (&content, doc_path) {
        (Some(md), _) => html! { article.prose { (render_markdown(strip_frontmatter(md))) } },
        (None, "index") => no_docs(app, ns, tp, ver, &all_versions, headers),
        (None, _) => html! { section.card { (empty("file", html! { "No page " code { (doc_path) } " in these docs." })) } },
    };
    let title = match doc_path {
        "index" => format!("{ns}/{tp}"),
        p => format!("{} · {ns}/{tp}", p.rsplit('/').next().unwrap_or(p)),
    };

    render(&shell, &title, html! {
        div.head {
            div.head-text {
                div.crumbs { a href="/registry" { "registry" } span.sep { "/" } span { (ns) } }
                h1 { span.mono { (tp) } }
            }
            div.head-meta {
                select aria-label="Version" onchange="location = this.value" {
                    @for v in all_versions.iter().rev() {
                        option value={"/registry/" (ns) "/" (tp) "/" (v) "/docs"} selected[v == ver] { "v" (v) }
                    }
                }
            }
        }
        @if has_docs {
            div.docs {
                nav.docs-nav aria-label="Documentation" {
                    input.filter type="text" placeholder="Filter…" data-filter="#docnav" aria-label="Filter pages" autocomplete="off";
                    div #docnav {
                        a href=(base) aria-current=[(doc_path == "index").then_some("page")] style="margin-top:8px" { "Overview" }
                        @for (label, dir, items) in &categories {
                            @if !items.is_empty() {
                                div data-group=(label) {
                                    div.cat { (label) }
                                    ul style="list-style:none;margin:0;padding:0" {
                                        @for item in items {
                                            @let path = format!("{dir}/{item}");
                                            li { a href={(base) "/" (path)} aria-current=[(doc_path == path).then_some("page")] { (item) } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div style="min-width:0" { (main) }
            }
        } @else {
            (main)
        }
    })
    .into_response()
}

fn no_docs(app: &AppState, ns: &str, tp: &str, ver: &str, versions: &[String], headers: &HeaderMap) -> Markup {
    let host = origin(headers);
    let host = host.trim_start_matches("https://").trim_start_matches("http://");
    let usage = format!(
        "terraform {{\n  required_providers {{\n    {tp} = {{\n      source  = \"{host}/{ns}/{tp}\"\n      version = \"{ver}\"\n    }}\n  }}\n}}"
    );
    html! {
        div.grid.two {
            section.card {
                div.card-h { h2 { (icon("file")) "Usage" } span.aside { "no docs uploaded for v" (ver) } }
                div.card-b {
                    (codeblock(&usage))
                    p.note { "Upload docs with " code { "PUT /registry/providers/" (ns) "/" (tp) "/" (ver) "/docs" } " — a ZIP of the " code { "docs/" } " directory from terraform-plugin-docs, or plain Markdown." }
                }
            }
            section.card {
                div.card-h { h2 { (icon("layers")) "Versions" } span.aside { (versions.len()) } }
                ul.rows {
                    @for v in versions.iter().rev() {
                        @let plats = platforms(app, ns, tp, v);
                        @let meta = app.registry.get_meta(ns, tp, v).unwrap_or_default();
                        li {
                            a.grow.row-link href={"/registry/" (ns) "/" (tp) "/" (v) "/docs"} {
                                div.title.mono { "v" (v) }
                                div.sub { (plats.join(" · ")) }
                            }
                            @if !meta.protocols.is_empty() { span.chip.ghost.mono { "proto " (meta.protocols.join(", ")) } }
                        }
                    }
                }
            }
        }
    }
}

fn platforms(app: &AppState, ns: &str, tp: &str, ver: &str) -> Vec<String> {
    let mut plats: Vec<String> = app.registry.list_platforms(ns, tp, ver).iter().map(|p| format!("{}_{}", p.os, p.arch)).collect();
    plats.sort();
    plats
}

/// Render provider docs. Raw HTML in the Markdown is shown as text and only
/// safe link schemes survive: these pages are public and anyone who can push
/// a provider can upload docs.
fn render_markdown(md: &str) -> Markup {
    use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};

    fn safe_url(url: &str) -> bool {
        let lower = url.trim().to_ascii_lowercase();
        match lower.split_once(':') {
            // No scheme, or the colon comes after a path/query/fragment start.
            None => true,
            Some((scheme, _)) if scheme.contains(['/', '?', '#']) => true,
            Some((scheme, _)) => matches!(scheme, "http" | "https" | "mailto"),
        }
    }

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let events = Parser::new_ext(md, opts).map(|event| match event {
        Event::Html(s) | Event::InlineHtml(s) => Event::Text(s),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) if !safe_url(&dest_url) => {
            Event::Start(Tag::Link { link_type, dest_url: CowStr::Borrowed("#"), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) if !safe_url(&dest_url) => {
            Event::Start(Tag::Image { link_type, dest_url: CowStr::Borrowed(""), title, id })
        }
        e => e,
    });
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events);
    PreEscaped(out)
}

/// Strip YAML frontmatter (`---…---`) from the start of a markdown file.
fn strip_frontmatter(md: &str) -> &str {
    let Some(s) = md.strip_prefix("---\r\n").or_else(|| md.strip_prefix("---\n")) else {
        return md;
    };
    match s.find("\n---") {
        Some(pos) => s[pos + 4..].trim_start_matches("\r\n").trim_start_matches('\n'),
        None => md,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_neutralises_html_and_script_links() {
        let html = render_markdown("<script>alert(1)</script>\n\n[x](javascript:alert(1)) [ok](https://a.b/c) [rel](../x)").0;
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains(r#"href="https://a.b/c""#), "{html}");
        assert!(html.contains(r#"href="../x""#), "{html}");
    }

    #[test]
    fn frontmatter_is_stripped() {
        assert_eq!(strip_frontmatter("---\ntitle: x\n---\n# Hi"), "# Hi");
        assert_eq!(strip_frontmatter("# Hi"), "# Hi");
    }
}
