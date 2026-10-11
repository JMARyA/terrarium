//! Page shell: document head, sidebar with the workspace tree, and the
//! command palette.

use std::collections::BTreeMap;

use maud::{DOCTYPE, Markup, PreEscaped, html};

use super::assets;
use super::components::{icon, logo};
use super::data::Health;
use crate::AppState;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Section {
    Overview,
    Workspace,
    Policies,
    Registry,
    Tokens,
    Help,
}

pub struct Shell<'a> {
    pub app: &'a AppState,
    pub user: Option<&'a str>,
    pub section: Section,
    /// The workspace being viewed, highlighted in the tree.
    pub current: Option<&'a str>,
}

impl<'a> Shell<'a> {
    pub fn new(app: &'a AppState, user: Option<&'a str>, section: Section) -> Self {
        Self { app, user, section, current: None }
    }

    pub fn at(mut self, workspace: &'a str) -> Self {
        self.current = Some(workspace);
        self
    }
}

/// Let htmx swap error pages too, so a boosted link to a 404 still lands on
/// the 404 page instead of silently doing nothing.
const HTMX_CONFIG: &str = r#"{"responseHandling":[{"code":"204","swap":false},{"code":"[23]..","swap":true},{"code":"[45]..","swap":true,"error":true}],"scrollIntoViewOnBoost":false}"#;

fn head(title: &str) -> Markup {
    html! {
        head {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width,initial-scale=1";
            meta name="color-scheme" content="dark";
            meta name="htmx-config" content=(HTMX_CONFIG);
            title { (title) " · terrarium" }
            link rel="icon" href=(PreEscaped(FAVICON));
            link rel="stylesheet" href=(assets::url("app.css"));
            script src=(assets::url("htmx.min.js")) defer {}
            script src=(assets::url("app.js")) defer {}
        }
    }
}

const FAVICON: &str = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32' fill='none' stroke='%23a8dc6c' stroke-width='2.4' stroke-linecap='round'%3E%3Cpath d='M7 14a9 9 0 0 1 18 0v10.5a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 7 24.5z'/%3E%3Cpath d='M16 22v-6'/%3E%3C/svg%3E";

/// A full page with the sidebar.
pub fn render(shell: &Shell, title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(title))
            body hx-boost="true" {
                div.shell {
                    (sidebar(shell))
                    main id="main" { div.wrap { (body) } }
                }
                @if shell.user.is_some() { (palette()) }
            }
        }
    }
}

/// A page without chrome (sign-in).
pub fn bare(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(title))
            body { (body) }
        }
    }
}

fn sidebar(shell: &Shell) -> Markup {
    let nav = |section: Section, href: &str, ic: &str, label: &str, count: Option<usize>| {
        html! {
            a href=(href) aria-current=[(shell.section == section).then_some("page")] {
                (icon(ic)) span { (label) }
                @if let Some(n) = count { span.count { (n) } }
            }
        }
    };
    html! {
        aside.side {
            div.side-top {
                a.brand href="/" { (logo()) "terrarium" small { "v" (env!("CARGO_PKG_VERSION")) } }
                @if shell.user.is_some() {
                    button.search-trigger type="button" data-open-palette aria-label="Search" {
                        (icon("search")) span.label { "Jump to…" } kbd { "Ctrl K" }
                    }
                }
                button.btn.quiet.sm.menu-toggle type="button" data-menu aria-label="Menu" { (icon("menu")) }
            }
            @if let Some(user) = shell.user {
                nav.nav.collapsible {
                    (nav(Section::Overview, "/", "overview", "Overview", None))
                    (nav(Section::Policies, "/policies", "policy", "Policies", Some(shell.app.policies.list().len())))
                    (nav(Section::Registry, "/registry", "registry", "Registry", None))
                    (nav(Section::Tokens, "/tokens", "key", "Tokens", None))
                    (nav(Section::Help, "/help", "help", "Help", None))
                }
                (workspace_tree(shell))
                div.side-foot.collapsible {
                    span.avatar { (user.chars().next().unwrap_or('?')) }
                    span.who { (user) }
                    form method="post" action="/logout" hx-boost="false" {
                        button.btn.quiet.sm type="submit" title="Sign out" aria-label="Sign out" { (icon("logout")) }
                    }
                }
            } @else {
                nav.nav.collapsible {
                    (nav(Section::Registry, "/registry", "registry", "Registry", None))
                    (nav(Section::Help, "/help", "help", "Help", None))
                }
                div style="flex:1" {}
                div.side-foot.collapsible {
                    a.btn.primary href="/login" style="width:100%" { "Sign in" }
                }
            }
        }
    }
}

// ── Workspace tree ──────────────────────────────────────────────────────

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    leaf: Option<String>,
}

impl Node {
    fn insert(&mut self, name: &str) {
        let mut node = self;
        for seg in name.split('/') {
            node = node.children.entry(seg.to_string()).or_default();
        }
        node.leaf = Some(name.to_string());
    }

    fn contains(&self, name: &str) -> bool {
        self.leaf.as_deref() == Some(name) || self.children.values().any(|c| c.contains(name))
    }
}

fn workspace_tree(shell: &Shell) -> Markup {
    let app = shell.app;
    let active = app.state.list(None, false);
    let archived = app.state.list(None, true);

    let mut root = Node::default();
    for n in &active {
        root.insert(n);
    }
    // Small fleets show everything; large ones open only the path to the
    // workspace being viewed.
    let open_all = active.len() <= 40;

    html! {
        div.side-label.collapsible { span { "Workspaces" } span { (active.len()) } }
        nav.tree.collapsible aria-label="Workspaces" {
            @if active.is_empty() {
                div.tree-empty { "Nothing pushed yet." }
            } @else {
                (render_nodes(&root, shell, open_all, false))
            }
            @if !archived.is_empty() {
                details style="margin-top:8px" open[archived.iter().any(|a| Some(a.as_str()) == shell.current)] {
                    summary { (icon("chevron")) "archived" span.faint { " " (archived.len()) } }
                    ul.archived {
                        @for n in &archived {
                            li { (leaf_link(shell, n, n, true)) }
                        }
                    }
                }
            }
        }
    }
}

fn render_nodes(node: &Node, shell: &Shell, open_all: bool, archived: bool) -> Markup {
    html! {
        ul {
            @for (seg, child) in &node.children {
                li {
                    @if child.children.is_empty() {
                        @if let Some(full) = &child.leaf { (leaf_link(shell, full, seg, archived)) }
                    } @else {
                        @let open = open_all || shell.current.is_some_and(|c| child.contains(c));
                        details open[open] {
                            summary { (icon("chevron")) (seg) "/" }
                            @if let Some(full) = &child.leaf {
                                ul { li { (leaf_link(shell, full, seg, archived)) } }
                            }
                            (render_nodes(child, shell, open_all, archived))
                        }
                    }
                }
            }
        }
    }
}

fn leaf_link(shell: &Shell, full: &str, label: &str, archived: bool) -> Markup {
    let health = Health::of(shell.app, full, archived);
    html! {
        a href={"/w/" (full)} title={(full) " — " (health.label())}
            aria-current=[(shell.current == Some(full)).then_some("page")] {
            span class={"dot " (health.dot())} {}
            span { (label) }
        }
    }
}

fn palette() -> Markup {
    html! {
        dialog #palette .palette aria-label="Jump to" {
            div.palette-input {
                (icon("search"))
                input type="text" name="q" placeholder="Jump to a workspace, page or provider…"
                    autocomplete="off" spellcheck="false"
                    hx-get="/ui/search" hx-trigger="input changed delay:80ms, search"
                    hx-target="#palette-results" hx-sync="this:replace";
                kbd { "esc" }
            }
            div #palette-results .palette-results role="listbox" {}
            div.palette-foot {
                span { kbd { "↑" } " " kbd { "↓" } " move" }
                span { kbd { "↵" } " open" }
                span { kbd { "/" } " or " kbd { "Ctrl K" } " anywhere" }
            }
        }
    }
}
