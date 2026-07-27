//! Thin, informational web UI served alongside the HTTP state API.
//!
//! Browser auth uses a `session` cookie (authur [`UserAuth`]/[`MaybeUser`]),
//! completely separate from the Basic-Auth path the Terraform backend uses.
//! Every view reads directly from the same [`AppState`] containers the API
//! handlers use — no duplicated logic, no internal HTTP.

use std::collections::BTreeMap;

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use authur::csrf::CSRF;
use authur::{MaybeUser, Sessions, UserAuth, session_id};
use chrono::{DateTime, Utc};

use crate::AppState;
use crate::statediff::{Change, StateDiff};
use crate::tfstate::TfState;

// ── Cookie helpers ──────────────────────────────────────────────────────

/// `; Secure` when terra runs behind TLS (set `TERRARIUM_TLS=1`). Off by
/// default so the cookie works over plain http during local testing.
fn cookie_secure() -> &'static str {
    match std::env::var("TERRARIUM_TLS") {
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => "; Secure",
        _ => "",
    }
}

fn set_cookie(token: &str) -> String {
    format!(
        "session={token}; HttpOnly{}; SameSite=Lax; Path=/; Max-Age=2592000",
        cookie_secure()
    )
}

fn clear_cookie() -> String {
    "session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0".to_string()
}

fn read_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .find_map(|kv| kv.trim().strip_prefix("session=").map(str::to_string))
}

/// Reject workspace names that could escape the data dir. Mirrors the
/// validation the state/lock API handlers apply.
fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') || name.contains('\\') {
        return false;
    }
    name.split('/')
        .all(|c| !c.is_empty() && c != "." && c != "..")
}

fn not_found(user: &str, name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        page(
            "Not found",
            Some(user),
            &format!(r#"<h1>{}</h1><div class="panel dim">No such workspace.</div>"#, esc(name)),
        ),
    )
        .into_response()
}

// ── Rendering helpers ───────────────────────────────────────────────────

/// Strip ANSI/CSI escape sequences. `facet-diff` formats with terminal colour,
/// which is meaningless (and ugly) in HTML.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                // CSI sequences end on a letter (we only ever see `…m`).
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_markdown(md: &str) -> String {
    let mut opts = pulldown_cmark::Options::empty();
    opts.insert(pulldown_cmark::Options::ENABLE_TABLES);
    opts.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
    let parser = pulldown_cmark::Parser::new_ext(md, opts);
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    out
}

/// Strip YAML frontmatter (`---…---`) from the start of a markdown file.
fn strip_frontmatter(md: &str) -> &str {
    let s = if md.starts_with("---\r\n") { &md[5..] } else if md.starts_with("---\n") { &md[4..] } else { return md; };
    if let Some(pos) = s.find("\n---") {
        let after = &s[pos + 4..];
        return after.trim_start_matches("\r\n").trim_start_matches('\n');
    }
    md
}

const STYLE: &str = r#"
:root { --bg:#0f1115; --panel:#171a21; --line:#262b36; --fg:#d7dce5; --dim:#8a93a6;
        --green:#5ec27a; --red:#e06c75; --yellow:#e5c07b; --cyan:#56b6c2; --accent:#7da9ff; }
* { box-sizing:border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace; }
a { color:var(--accent); text-decoration:none; } a:hover { text-decoration:underline; }
header { display:flex; align-items:center; gap:20px; padding:12px 24px; border-bottom:1px solid var(--line); background:var(--panel); }
header .brand { font-weight:bold; } header nav { display:flex; gap:16px; } header .spacer { flex:1; }
header form { margin:0; } header button { background:none; border:1px solid var(--line); color:var(--dim); padding:4px 10px; border-radius:6px; cursor:pointer; }
main { max-width:900px; margin:0 auto; padding:24px; }
h1 { font-size:18px; } h2 { font-size:15px; color:var(--dim); margin-top:28px; text-transform:uppercase; letter-spacing:.05em; }
.panel { background:var(--panel); border:1px solid var(--line); border-radius:8px; padding:16px 18px; margin:12px 0; }
table { width:100%; border-collapse:collapse; } th,td { text-align:left; padding:6px 10px; border-bottom:1px solid var(--line); }
th { color:var(--dim); font-weight:normal; } tr:last-child td { border-bottom:none; }
.dim { color:var(--dim); } .green { color:var(--green); } .red { color:var(--red); } .yellow { color:var(--yellow); } .cyan { color:var(--cyan); }
.badge { font-size:12px; padding:2px 8px; border-radius:10px; border:1px solid var(--line); }
.badge.lock { color:var(--yellow); border-color:var(--yellow); } .badge.archived { color:var(--dim); }
ul.tree { list-style:none; padding-left:18px; } ul.tree.root { padding-left:0; }
ul.tree li { padding:2px 0; } .ns { color:var(--dim); }
input,select { background:var(--bg); border:1px solid var(--line); color:var(--fg); padding:6px 8px; border-radius:6px; font:inherit; }
button.primary { background:var(--accent); border:none; color:#0b1020; padding:7px 14px; border-radius:6px; cursor:pointer; font:inherit; font-weight:bold; }
button.danger { background:none; border:1px solid var(--red); color:var(--red); padding:4px 10px; border-radius:6px; cursor:pointer; font:inherit; }
pre.diff { background:var(--bg); border:1px solid var(--line); border-radius:6px; padding:12px; overflow-x:auto; white-space:pre; }
.token { background:var(--bg); border:1px dashed var(--yellow); border-radius:6px; padding:12px; word-break:break-all; color:var(--yellow); }
.err { color:var(--red); margin:8px 0; }
form.inline { display:flex; gap:10px; align-items:center; flex-wrap:wrap; }
/* Docs layout */
.docs-layout { display:flex; gap:0; }
.docs-sidebar { width:230px; flex-shrink:0; border-right:1px solid var(--line); padding:20px 0; min-height:calc(100vh - 57px); }
.docs-sidebar .ds-provider { font-weight:bold; padding:0 16px 12px; border-bottom:1px solid var(--line); margin-bottom:8px; }
.docs-sidebar .ds-provider a { color:var(--fg); }
.docs-sidebar select { display:block; width:calc(100% - 32px); margin:8px 16px 12px; }
.docs-sidebar .ds-cat { padding:10px 16px 4px; font-size:11px; letter-spacing:.07em; text-transform:uppercase; color:var(--dim); }
.docs-sidebar .ds-link { padding:3px 16px; }
.docs-sidebar .ds-link a { display:block; color:var(--dim); font-size:13px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
.docs-sidebar .ds-link a:hover { color:var(--fg); text-decoration:none; }
.docs-sidebar .ds-link a.cur { color:var(--accent); font-weight:bold; }
.docs-main { flex:1; min-width:0; padding:28px 36px; }
/* Markdown rendering */
.md h1 { font-size:22px; margin:0 0 16px; border-bottom:1px solid var(--line); padding-bottom:10px; }
.md h2 { font-size:16px; color:var(--fg); margin:28px 0 10px; text-transform:none; letter-spacing:0; border-bottom:1px solid var(--line); padding-bottom:6px; }
.md h3 { font-size:14px; color:var(--fg); margin:20px 0 8px; }
.md h4 { font-size:13px; color:var(--dim); margin:14px 0 6px; }
.md p { margin:10px 0; line-height:1.7; }
.md code { background:var(--panel); border:1px solid var(--line); border-radius:3px; padding:1px 5px; font:inherit; font-size:12px; }
.md pre { background:var(--panel); border:1px solid var(--line); border-radius:6px; padding:14px 16px; overflow-x:auto; margin:12px 0; }
.md pre code { background:none; border:none; padding:0; font-size:13px; }
.md ul, .md ol { padding-left:22px; margin:10px 0; line-height:1.8; }
.md li { margin:2px 0; }
.md blockquote { border-left:3px solid var(--accent); margin:12px 0; padding:4px 16px; color:var(--dim); }
.md table { width:100%; border-collapse:collapse; margin:14px 0; }
.md th { background:var(--panel); color:var(--dim); font-weight:normal; }
.md th, .md td { text-align:left; padding:7px 12px; border:1px solid var(--line); font-size:13px; }
.md a { color:var(--accent); }
.md hr { border:none; border-top:1px solid var(--line); margin:20px 0; }
.md strong { color:var(--fg); }
"#;

fn page(title: &str, user: Option<&str>, body: &str) -> Html<String> {
    let nav = match user {
        Some(u) => format!(
            r#"<nav><a href="/">Workspaces</a><a href="/registry">Registry</a><a href="/tokens">Tokens</a><a href="/help">Help</a></nav>
               <span class="spacer"></span>
               <span class="dim">{}</span>
               <form method="post" action="/logout"><button type="submit">logout</button></form>"#,
            esc(u)
        ),
        None => r#"<nav><a href="/help">Help</a></nav>"#.to_string(),
    };
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{} — terrarium</title><style>{}</style></head><body>
<header><span class="brand">🌱 terrarium</span>{}</header>
<main>{}</main></body></html>"#,
        esc(title),
        STYLE,
        nav,
        body
    ))
}

/// Full-width page for the docs layout (no `max-width` on `<main>`).
fn page_docs(title: &str, user: Option<&str>, body: &str) -> Html<String> {
    let nav = match user {
        Some(u) => format!(
            r#"<nav><a href="/">Workspaces</a><a href="/registry">Registry</a><a href="/tokens">Tokens</a><a href="/help">Help</a></nav>
               <span class="spacer"></span>
               <span class="dim">{}</span>
               <form method="post" action="/logout"><button type="submit">logout</button></form>"#,
            esc(u)
        ),
        None => r#"<nav><a href="/registry">Registry</a><a href="/help">Help</a></nav>"#.to_string(),
    };
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{} — terrarium</title><style>{}</style></head><body>
<header><span class="brand">🌱 terrarium</span>{}</header>
<div style="min-height:calc(100vh - 57px)">{}</div></body></html>"#,
        esc(title),
        STYLE,
        nav,
        body
    ))
}

fn require_login() -> Response {
    Redirect::to("/login").into_response()
}

// ── Auth: login / logout ────────────────────────────────────────────────

pub async fn login_page(maybe: MaybeUser) -> Response {
    if maybe.user().is_some() {
        return Redirect::to("/").into_response();
    }
    login_form(None).into_response()
}

fn login_form(error: Option<&str>) -> Html<String> {
    let err = error
        .map(|e| format!(r#"<p class="err">{}</p>"#, esc(e)))
        .unwrap_or_default();
    let body = format!(
        r#"<h1>Sign in</h1>
<div class="panel" style="max-width:360px">
  <form method="post" action="/login">
    {err}
    <p><label>Username<br><input name="username" autofocus style="width:100%"></label></p>
    <p><label>Password<br><input name="password" type="password" style="width:100%"></label></p>
    <button class="primary" type="submit">Sign in</button>
  </form>
</div>"#
    );
    page("Sign in", None, &body)
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
            (
                [(header::SET_COOKIE, set_cookie(&session.token))],
                Redirect::to("/"),
            )
                .into_response()
        }
        None => {
            metrics::counter!("terrarium_auth_logins_total", "result" => "failure").increment(1);
            (
                StatusCode::UNAUTHORIZED,
                login_form(Some("Invalid username or password")),
            )
                .into_response()
        }
    }
}

pub async fn logout(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = read_session_cookie(&headers) {
        app.users.end_session(&session_id(&token)).await;
    }
    (
        [(header::SET_COOKIE, clear_cookie())],
        Redirect::to("/login"),
    )
        .into_response()
}

// ── Dashboard: workspace namespace tree ─────────────────────────────────

#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    /// Full workspace name, set on leaf nodes.
    leaf: Option<String>,
}

impl TreeNode {
    fn insert(&mut self, name: &str) {
        let mut node = self;
        for seg in name.split('/') {
            node = node.children.entry(seg.to_string()).or_default();
        }
        node.leaf = Some(name.to_string());
    }

    fn render(&self, locks: &std::collections::HashMap<String, crate::lock::LockInfo>, out: &mut String, root: bool) {
        out.push_str(if root {
            "<ul class=\"tree root\">"
        } else {
            "<ul class=\"tree\">"
        });
        for (seg, child) in &self.children {
            out.push_str("<li>");
            match &child.leaf {
                Some(full) => {
                    let lock = if locks.contains_key(full) {
                        r#" <span class="badge lock">locked</span>"#
                    } else {
                        ""
                    };
                    out.push_str(&format!(
                        r#"<a href="/w/{}">{}</a>{}"#,
                        esc(full),
                        esc(seg),
                        lock
                    ));
                }
                None => out.push_str(&format!(r#"<span class="ns">{}/</span>"#, esc(seg))),
            }
            if !child.children.is_empty() {
                child.render(locks, out, false);
            }
            out.push_str("</li>");
        }
        out.push_str("</ul>");
    }
}

pub async fn dashboard(maybe: MaybeUser, State(app): State<AppState>) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };

    let names = app.state.list(None, false);
    let archived = app.state.list(None, true);
    let locks = app.locks.list();

    let mut body = format!(
        r#"<h1>Workspaces <span class="dim">({})</span></h1>"#,
        names.len()
    );

    if names.is_empty() {
        body.push_str(r#"<div class="panel dim">No state yet.</div>"#);
    } else {
        let mut tree = TreeNode::default();
        for n in &names {
            tree.insert(n);
        }
        body.push_str(r#"<div class="panel">"#);
        tree.render(&locks, &mut body, true);
        body.push_str("</div>");
    }

    body.push_str(&format!(
        r#"<h2>Locks <span class="dim">({})</span></h2>"#,
        locks.len()
    ));
    if locks.is_empty() {
        body.push_str(r#"<div class="panel dim">No active locks.</div>"#);
    } else {
        body.push_str(r#"<div class="panel"><table><tr><th>Workspace</th><th>Who</th><th>Lock ID</th><th>Since</th></tr>"#);
        let mut entries: Vec<_> = locks.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (name, info) in entries {
            body.push_str(&format!(
                r#"<tr><td><a href="/w/{}">{}</a></td><td class="yellow">{}</td><td class="cyan">{}</td><td class="dim">{}</td></tr>"#,
                esc(name),
                esc(name),
                esc(info.Who.as_deref().unwrap_or("unknown")),
                esc(&info.ID),
                esc(info.Created.as_deref().unwrap_or("")),
            ));
        }
        body.push_str("</table></div>");
    }

    if !archived.is_empty() {
        body.push_str(r#"<h2>Archived</h2><div class="panel"><ul class="tree root">"#);
        for n in &archived {
            body.push_str(&format!(
                r#"<li><a href="/w/{}">{}</a> <span class="badge archived">read-only</span></li>"#,
                esc(n),
                esc(n)
            ));
        }
        body.push_str("</ul></div>");
    }

    // Policy violations — placeholder until OPA lands.
    body.push_str(r#"<h2>Policy violations</h2><div class="panel dim">No policy engine configured. (OPA integration pending.)</div>"#);

    page("Workspaces", Some(&user.username), &body).into_response()
}

// ── Per-workspace view ──────────────────────────────────────────────────

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub async fn workspace(maybe: MaybeUser, State(app): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return not_found(&user.username, &name);
    }

    let versions = app.state.list_versions(&name);
    if versions.is_empty() && app.state.get(&name).is_none() {
        return (StatusCode::NOT_FOUND, page("Not found", Some(&user.username), &format!(r#"<h1>{}</h1><div class="panel dim">No such workspace.</div>"#, esc(&name)))).into_response();
    }

    let archived = app.state.is_archived(&name);
    let lock = app.locks.get(&name);
    let current = versions.iter().max().copied();

    let archived_badge = if archived {
        r#" <span class="badge archived">read-only</span>"#
    } else {
        ""
    };
    let mut body = format!("<h1>{}{}</h1>", esc(&name), archived_badge);
    if app.state.get(&name).is_some() {
        body.push_str(&format!(
            r#"<p><a href="/graph/{}">↳ state overview &amp; dependency graph</a></p>"#,
            esc(&name)
        ));
    }

    // Lock status
    body.push_str(r#"<h2>Lock status</h2><div class="panel">"#);
    match &lock {
        Some(info) => body.push_str(&format!(
            r#"<span class="badge lock">locked</span> by <span class="yellow">{}</span> <span class="dim">— lock ID</span> <span class="cyan">{}</span><br><span class="dim">since {}</span>"#,
            esc(info.Who.as_deref().unwrap_or("unknown")),
            esc(&info.ID),
            esc(info.Created.as_deref().unwrap_or("")),
        )),
        None => body.push_str(r#"<span class="green">unlocked</span>"#),
    }
    body.push_str("</div>");

    // Version history + size trend
    body.push_str(&format!(
        r#"<h2>Version history <span class="dim">({})</span></h2>"#,
        versions.len()
    ));
    if versions.is_empty() {
        body.push_str(r#"<div class="panel dim">No versions recorded.</div>"#);
    } else {
        let mut max_size = 1u64;
        let mut rows = Vec::new();
        for v in &versions {
            let path = app.state.versions_dir.join(&name).join(v.to_string());
            let meta = std::fs::metadata(&path).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            max_size = max_size.max(size);
            let when = meta
                .and_then(|m| m.modified().ok())
                .map(|t| DateTime::<Utc>::from(t).format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            rows.push((*v, size, when));
        }
        body.push_str(r#"<div class="panel"><table><tr><th>Version</th><th>Size</th><th>Trend</th><th>Pushed</th></tr>"#);
        for (v, size, when) in rows.iter().rev() {
            let cur = if Some(*v) == current {
                r#" <span class="green">(current)</span>"#
            } else {
                ""
            };
            let width = (*size as f64 / max_size as f64 * 120.0).round() as u64;
            body.push_str(&format!(
                r#"<tr><td><a href="/graph/{}?version={}" class="cyan">v{}</a>{}</td><td>{}</td><td><span style="display:inline-block;height:8px;width:{}px;background:var(--accent);border-radius:2px"></span></td><td class="dim">{}</td></tr>"#,
                esc(&name),
                v,
                v,
                cur,
                fmt_size(*size),
                width,
                esc(when),
            ));
        }
        body.push_str("</table></div>");

        // Diff form
        if versions.len() >= 2 {
            let opts = |sel: u32| {
                versions
                    .iter()
                    .map(|v| {
                        format!(
                            r#"<option value="{v}"{}>v{v}</option>"#,
                            if *v == sel { " selected" } else { "" }
                        )
                    })
                    .collect::<String>()
            };
            let from_default = versions[versions.len() - 2];
            let to_default = versions[versions.len() - 1];
            body.push_str(&format!(
                r#"<h2>Compare versions</h2><div class="panel"><form class="inline" method="get" action="/diff/{}">
                   <label>from <select name="from">{}</select></label>
                   <label>to <select name="to">{}</select></label>
                   <button class="primary" type="submit">Diff</button></form></div>"#,
                esc(&name),
                opts(from_default),
                opts(to_default),
            ));
        }
    }

    // Activity log — derived from persisted lock history (who locked, for which
    // operation, when). This is the closest thing to a push/audit trail terra
    // records today; pushes made without a lock won't appear.
    let activity = app.locks.history(&name);
    body.push_str(&format!(
        r#"<h2>Activity <span class="dim">({})</span></h2>"#,
        activity.len()
    ));
    if activity.is_empty() {
        body.push_str(r#"<div class="panel dim">No lock activity recorded.</div>"#);
    } else {
        body.push_str(r#"<div class="panel"><table><tr><th>When</th><th>Who</th><th>Operation</th><th>Terraform</th><th>Info</th></tr>"#);
        for info in activity.iter().take(50) {
            body.push_str(&format!(
                r#"<tr><td class="dim">{}</td><td class="yellow">{}</td><td>{}</td><td class="dim">{}</td><td class="dim">{}</td></tr>"#,
                esc(info.Created.as_deref().unwrap_or("")),
                esc(info.Who.as_deref().unwrap_or("unknown")),
                esc(info.Operation.as_deref().unwrap_or("—")),
                esc(info.Version.as_deref().unwrap_or("—")),
                esc(info.Info.as_deref().unwrap_or("")),
            ));
        }
        body.push_str(r#"</table><p class="dim">Derived from lock history — pushes made without a lock are not shown.</p></div>"#);
    }

    // Webhooks for this workspace
    let hooks = app.webhooks.list_for(&name).await;
    body.push_str(r#"<h2>Webhooks</h2>"#);
    if hooks.is_empty() {
        body.push_str(r#"<div class="panel dim">None.</div>"#);
    } else {
        body.push_str(r#"<div class="panel"><table><tr><th>ID</th><th>URL</th><th>Events</th></tr>"#);
        for h in hooks {
            let events = if h.events.is_empty() {
                "all".to_string()
            } else {
                h.events.join(", ")
            };
            body.push_str(&format!(
                r#"<tr><td class="cyan">{}</td><td>{}</td><td class="dim">{}</td></tr>"#,
                esc(&h.id),
                esc(&h.url),
                esc(&events)
            ));
        }
        body.push_str("</table></div>");
    }

    page(&name, Some(&user.username), &body).into_response()
}

// ── Diff view ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct DiffQuery {
    from: u32,
    to: u32,
}

pub async fn diff_view(maybe: MaybeUser, State(app): State<AppState>, Path(name): Path<String>, Query(q): Query<DiffQuery>) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return not_found(&user.username, &name);
    }

    let from = app.state.get_version(&name, q.from);
    let to = app.state.get_version(&name, q.to);
    let (Some(from), Some(to)) = (from, to) else {
        return (StatusCode::NOT_FOUND, page("Diff", Some(&user.username), r#"<div class="panel dim">One of the requested versions does not exist.</div>"#)).into_response();
    };

    let mut body = format!(
        r#"<h1>Diff <span class="dim">{}</span> <span class="cyan">v{}</span> → <span class="cyan">v{}</span></h1>
           <p><a href="/w/{}">← back to workspace</a></p>"#,
        esc(&name),
        q.from,
        q.to,
        esc(&name)
    );

    let diff = crate::statediff::diff_states(&from, &to);
    if diff.is_empty() {
        body.push_str(r#"<div class="panel dim">No changes.</div>"#);
        return page("Diff", Some(&user.username), &body).into_response();
    }

    match diff {
        StateDiff::Structured { terraform_version, serial, changes } => {
            let mut pre = String::new();
            if let Some((a, b)) = &terraform_version {
                pre.push_str(&format!("terraform: {} → {}\n", esc(a), esc(b)));
            }
            if let Some((a, b)) = &serial {
                pre.push_str(&format!("serial:    {a} → {b}\n"));
            }
            if !pre.is_empty() {
                body.push_str(&format!(r#"<pre class="diff">{pre}</pre>"#));
            }
            for change in &changes {
                match change {
                    Change::Added(addr) => body.push_str(&format!(
                        r#"<div class="panel"><span class="green">+ {}</span></div>"#,
                        esc(addr)
                    )),
                    Change::Removed(addr) => body.push_str(&format!(
                        r#"<div class="panel"><span class="red">- {}</span></div>"#,
                        esc(addr)
                    )),
                    Change::Modified { addr, instances } => {
                        let mut detail = String::new();
                        for inst in instances {
                            if inst.multi {
                                detail.push_str(&format!("[{}]\n", inst.index));
                            }
                            detail.push_str(&esc(&strip_ansi(&inst.diff)));
                            detail.push('\n');
                        }
                        body.push_str(&format!(
                            r#"<div class="panel"><div class="yellow">~ {}</div><pre class="diff">{}</pre></div>"#,
                            esc(addr),
                            detail
                        ));
                    }
                }
            }
        }
        StateDiff::Raw(out) => {
            body.push_str(&format!(r#"<pre class="diff">{}</pre>"#, esc(&strip_ansi(&out))));
        }
        StateDiff::Error(e) => {
            body.push_str(&format!(r#"<div class="err">{}</div>"#, esc(&e)));
        }
    }

    page("Diff", Some(&user.username), &body).into_response()
}

// ── Self-service token management ───────────────────────────────────────

pub async fn tokens_page(UserAuth(user): UserAuth, State(app): State<AppState>) -> Response {
    let csrf = app.users.get_csrf(&user.username).await;
    let sessions = app.users.list_sessions(&user.username).await;

    let mut body = String::from("<h1>Sessions &amp; API tokens</h1>");
    body.push_str(&format!(
        r#"<div class="panel"><form class="inline" method="post" action="/tokens">
           <input type="hidden" name="csrf" value="{}">
           <label>New API token <input name="name" placeholder="ci-deploy" required></label>
           <button class="primary" type="submit">Create</button></form></div>"#,
        esc(&csrf)
    ));

    if sessions.is_empty() {
        body.push_str(r#"<div class="panel dim">No active sessions or tokens.</div>"#);
    } else {
        body.push_str(r#"<div class="panel"><table><tr><th>Name</th><th>Kind</th><th>Created</th><th></th></tr>"#);
        for s in &sessions {
            let name = s.name.clone().unwrap_or_else(|| "—".to_string());
            let created = s.created.format("%Y-%m-%d %H:%M UTC").to_string();
            body.push_str(&format!(
                r#"<tr><td>{}</td><td class="dim">{:?}</td><td class="dim">{}</td>
                   <td><form method="post" action="/tokens/{}/revoke">
                   <input type="hidden" name="csrf" value="{}">
                   <button class="danger" type="submit">revoke</button></form></td></tr>"#,
                esc(&name),
                s.kind,
                esc(&created),
                esc(&s.id),
                esc(&csrf),
            ));
        }
        body.push_str("</table></div>");
    }

    body.push_str(r#"<p class="dim">API tokens authenticate as <code>Authorization: Bearer &lt;token&gt;</code>.</p>"#);
    page("Tokens", Some(&user.username), &body).into_response()
}

#[derive(serde::Deserialize)]
pub struct CreateTokenForm {
    name: String,
    csrf: String,
}

pub async fn token_create(UserAuth(user): UserAuth, State(app): State<AppState>, Form(f): Form<CreateTokenForm>) -> Response {
    if !app.users.verify_csrf(&f.csrf, &user.username).await {
        metrics::counter!("terrarium_auth_csrf_failures_total", "action" => "token_create").increment(1);
        return (StatusCode::FORBIDDEN, page("Tokens", Some(&user.username), r#"<div class="err">Invalid CSRF token — reload and try again.</div>"#)).into_response();
    }
    let session = app.users.api_key(f.name.trim(), &user.username).await;
    metrics::counter!("terrarium_auth_token_operations_total", "action" => "create").increment(1);
    let body = format!(
        r#"<h1>API token created</h1>
        <div class="panel"><p>Copy this token now — it will not be shown again.</p>
        <div class="token">{}</div></div>
        <p><a href="/tokens">← back to tokens</a></p>"#,
        esc(&session.token)
    );
    page("Tokens", Some(&user.username), &body).into_response()
}

#[derive(serde::Deserialize)]
pub struct RevokeForm {
    csrf: String,
}

pub async fn token_revoke(UserAuth(user): UserAuth, State(app): State<AppState>, Path(id): Path<String>, Form(f): Form<RevokeForm>) -> Response {
    if !app.users.verify_csrf(&f.csrf, &user.username).await {
        metrics::counter!("terrarium_auth_csrf_failures_total", "action" => "token_revoke").increment(1);
        return (StatusCode::FORBIDDEN, page("Tokens", Some(&user.username), r#"<div class="err">Invalid CSRF token — reload and try again.</div>"#)).into_response();
    }
    // Only allow revoking the caller's own sessions.
    let owned = app
        .users
        .list_sessions(&user.username)
        .await
        .into_iter()
        .any(|s| s.id == id);
    if owned {
        app.users.end_session(&id).await;
        metrics::counter!("terrarium_auth_token_operations_total", "action" => "revoke").increment(1);
    }
    Redirect::to("/tokens").into_response()
}

// ── State visualization: resource dependency graph ──────────────────────

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}

/// Render the current state's resources as a layered dependency graph (SVG).
///
/// Nodes are resource addresses; edges run from a dependency to the resource
/// that depends on it. Layout is a simple longest-path layering — depended-upon
/// resources sit to the left, dependents to the right.
fn render_state_graph(state: &TfState) -> String {
    use std::collections::{BTreeSet, HashMap};

    let mut modes: BTreeMap<String, String> = BTreeMap::new();
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &state.resources {
        let addr = r.address();
        modes.insert(addr.clone(), r.mode.clone());
        let entry = deps.entry(addr.clone()).or_default();
        for inst in &r.instances {
            if let Some(d) = &inst.dependencies {
                for dep in d {
                    entry.insert(dep.clone());
                }
            }
        }
    }

    let nodes: Vec<String> = modes.keys().cloned().collect();
    if nodes.is_empty() {
        return r#"<p class="dim">No resources in this state.</p>"#.to_string();
    }

    // Keep only edges that point at resources actually present in the state.
    let known: BTreeSet<String> = modes.keys().cloned().collect();
    for set in deps.values_mut() {
        set.retain(|d| known.contains(d));
    }

    // Longest-path depth per node, with a cycle guard.
    fn depth(
        addr: &str,
        deps: &BTreeMap<String, BTreeSet<String>>,
        memo: &mut HashMap<String, usize>,
        stack: &mut BTreeSet<String>,
    ) -> usize {
        if let Some(d) = memo.get(addr) {
            return *d;
        }
        if stack.contains(addr) {
            return 0; // break dependency cycle
        }
        stack.insert(addr.to_string());
        let d = deps
            .get(addr)
            .map(|s| {
                s.iter()
                    .map(|dep| depth(dep, deps, memo, stack) + 1)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        stack.remove(addr);
        memo.insert(addr.to_string(), d);
        d
    }

    let mut memo = HashMap::new();
    let mut layers: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for n in &nodes {
        let mut stack = BTreeSet::new();
        let l = depth(n, &deps, &mut memo, &mut stack);
        layers.entry(l).or_default().push(n.clone());
    }

    const COL_W: usize = 260;
    const ROW_H: usize = 64;
    const BOX_W: usize = 210;
    const BOX_H: usize = 38;
    const PAD: usize = 24;

    let mut pos: HashMap<String, (usize, usize)> = HashMap::new();
    let mut max_rows = 0usize;
    for (layer, members) in &layers {
        max_rows = max_rows.max(members.len());
        for (row, addr) in members.iter().enumerate() {
            pos.insert(addr.clone(), (PAD + layer * COL_W, PAD + row * ROW_H));
        }
    }
    let width = PAD * 2 + layers.len().max(1) * COL_W;
    let height = PAD * 2 + max_rows.max(1) * ROW_H;

    let mut svg = format!(
        r##"<svg viewBox="0 0 {w} {h}" width="{w}" height="{h}" xmlns="http://www.w3.org/2000/svg" font-family="ui-monospace,Menlo,monospace" font-size="11">
<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
<path d="M0,0 L10,5 L0,10 z" fill="#7da9ff"/></marker></defs>"##,
        w = width,
        h = height
    );

    // Edges first, so nodes draw on top. Arrowhead points dependency → dependent.
    for (addr, set) in &deps {
        let Some(&(nx, ny)) = pos.get(addr) else { continue };
        for dep in set {
            let Some(&(dx, dy)) = pos.get(dep) else { continue };
            let (x1, y1) = (dx + BOX_W, dy + BOX_H / 2);
            // Stop a few px short of the node so the arrowhead sits at its edge.
            let (x2, y2) = (nx.saturating_sub(3), ny + BOX_H / 2);
            let mid = (x1 + x2) / 2;
            svg.push_str(&format!(
                r##"<path d="M{x1},{y1} C{mid},{y1} {mid},{y2} {x2},{y2}" fill="none" stroke="#5b6675" stroke-width="1.5" marker-end="url(#arrow)"/>"##
            ));
        }
    }

    // Nodes.
    for addr in &nodes {
        let &(x, y) = pos.get(addr).unwrap();
        let is_data = modes.get(addr).map(|m| m == "data").unwrap_or(false);
        let stroke = if is_data { "var(--yellow)" } else { "var(--accent)" };
        svg.push_str(&format!(
            r#"<g><title>{full}</title><rect x="{x}" y="{y}" width="{bw}" height="{bh}" rx="7" fill="var(--panel)" stroke="{stroke}" stroke-width="1.5"/><text x="{tx}" y="{ty}" fill="var(--fg)">{label}</text></g>"#,
            full = esc(addr),
            x = x,
            y = y,
            bw = BOX_W,
            bh = BOX_H,
            stroke = stroke,
            tx = x + 10,
            ty = y + BOX_H / 2 + 4,
            label = esc(&truncate(addr, 28)),
        ));
    }

    svg.push_str("</svg>");
    svg
}

/// Resource/output overview grouped by type — the "what's in this state" panel.
fn render_state_overview(state: &TfState) -> String {
    let mut managed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut data: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut instance_count = 0usize;
    for r in &state.resources {
        instance_count += r.instances.len();
        let inst_name = match &r.module {
            Some(m) => format!("{}.{}", m, r.name),
            None => r.name.clone(),
        };
        let bucket = if r.mode == "data" { &mut data } else { &mut managed };
        bucket.entry(r.type_.clone()).or_default().push(inst_name);
    }

    let mut out = String::new();

    // Metadata
    out.push_str(&format!(
        r#"<div class="panel"><table>
        <tr><th>Terraform</th><td class="cyan">{}</td></tr>
        <tr><th>Serial</th><td class="cyan">{}</td></tr>
        <tr><th>Lineage</th><td class="dim">{}</td></tr>
        <tr><th>Resources</th><td>{} <span class="dim">({} instances)</span></td></tr>
        <tr><th>Outputs</th><td>{}</td></tr></table></div>"#,
        esc(&state.terraform_version),
        state.serial,
        esc(&state.lineage),
        state.resources.len(),
        instance_count,
        state.outputs.len(),
    ));

    let render_group = |title: &str, groups: &BTreeMap<String, Vec<String>>, stroke: &str| -> String {
        if groups.is_empty() {
            return String::new();
        }
        let total: usize = groups.values().map(|v| v.len()).sum();
        let mut s = format!(
            r#"<h2>{} <span class="dim">({})</span></h2><div class="panel">"#,
            esc(title), total
        );
        for (ty, names) in groups {
            s.push_str(&format!(
                r#"<div style="margin:10px 0"><span class="badge" style="border-color:{}">{}</span> <span class="dim">×{}</span><ul class="tree root">"#,
                stroke,
                esc(ty),
                names.len()
            ));
            for n in names {
                s.push_str(&format!("<li>{}</li>", esc(n)));
            }
            s.push_str("</ul></div>");
        }
        s.push_str("</div>");
        s
    };

    out.push_str(&render_group("Managed resources", &managed, "var(--accent)"));
    out.push_str(&render_group("Data sources", &data, "var(--yellow)"));

    // Outputs
    if !state.outputs.is_empty() {
        let mut keys: Vec<&String> = state.outputs.keys().collect();
        keys.sort();
        out.push_str(r#"<h2>Outputs</h2><div class="panel"><table><tr><th>Name</th><th>Sensitive</th></tr>"#);
        for k in keys {
            let sensitive = state.outputs[k].sensitive;
            out.push_str(&format!(
                r#"<tr><td>{}</td><td class="{}">{}</td></tr>"#,
                esc(k),
                if sensitive { "yellow" } else { "dim" },
                if sensitive { "yes" } else { "no" },
            ));
        }
        out.push_str("</table></div>");
    }

    out
}

#[derive(serde::Deserialize)]
pub struct VersionQuery {
    version: Option<u32>,
}

pub async fn graph_view(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<VersionQuery>,
) -> Response {
    let Some(user) = maybe.user() else {
        return require_login();
    };
    if !valid_name(&name) {
        return not_found(&user.username, &name);
    }

    let versions = app.state.list_versions(&name);
    let current = versions.iter().max().copied();
    // Default to the current version when none is requested.
    let selected = q.version.or(current);

    let data = match selected {
        Some(v) => app.state.get_version(&name, v),
        None => app.state.get(&name),
    };
    let Some(data) = data else {
        return (
            StatusCode::NOT_FOUND,
            page(
                "State",
                Some(&user.username),
                &format!(r#"<h1>{}</h1><div class="panel dim">No such workspace or version.</div>"#, esc(&name)),
            ),
        )
            .into_response();
    };

    let sel_label = selected.map(|v| format!("v{v}")).unwrap_or_default();
    let mut body = format!(
        r#"<h1>State <span class="dim">{}</span> <span class="cyan">{}</span></h1><p><a href="/w/{}">← back to workspace</a></p>"#,
        esc(&name),
        esc(&sel_label),
        esc(&name)
    );

    // Version selector
    if !versions.is_empty() {
        let opts: String = versions
            .iter()
            .rev()
            .map(|v| {
                let cur = if Some(*v) == current { " (current)" } else { "" };
                format!(
                    r#"<option value="{v}"{}>v{v}{}</option>"#,
                    if Some(*v) == selected { " selected" } else { "" },
                    cur
                )
            })
            .collect();
        body.push_str(&format!(
            r#"<div class="panel"><form class="inline" method="get" action="/graph/{}">
               <label>version <select name="version" onchange="this.form.submit()">{}</select></label>
               <noscript><button class="primary" type="submit">Show</button></noscript></form></div>"#,
            esc(&name),
            opts
        ));
    }

    match facet_json::from_str::<TfState>(&String::from_utf8_lossy(&data)) {
        Ok(state) => {
            body.push_str(&render_state_overview(&state));
            body.push_str(&format!(
                r#"<h2>Dependency graph</h2>
                <p class="dim"><span class="cyan">accent</span> = managed · <span class="yellow">yellow</span> = data source · arrows point dependency → dependent</p>
                <div class="panel" style="overflow:auto">{}</div>"#,
                render_state_graph(&state)
            ));
        }
        Err(_) => body.push_str(r#"<div class="panel dim">This version could not be parsed as Terraform state v4.</div>"#),
    }

    page("State", Some(&user.username), &body).into_response()
}

// ── Registry UI ─────────────────────────────────────────────────────────────

/// `GET /registry` — list all providers stored in this terrarium instance.
pub async fn registry_page(
    State(app): State<AppState>,
    maybe: MaybeUser,
) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    let providers = app.registry.list_providers();

    let mut rows = String::new();
    for (ns, tp) in &providers {
        let versions = app.registry.list_versions(ns, tp);
        let latest = versions.last().cloned().unwrap_or_default();
        let all_platforms: Vec<_> = versions.iter().flat_map(|v| app.registry.list_platforms(ns, tp, v)).collect();
        let mut seen = std::collections::BTreeSet::new();
        let plats: Vec<String> = all_platforms.into_iter()
            .filter_map(|p| {
                let k = format!("{}_{}", p.os, p.arch);
                seen.insert(k.clone()).then_some(k)
            })
            .collect();
        let has_docs = !latest.is_empty() && app.registry.has_docs(ns, tp, &latest);
        let docs_badge = if has_docs { r#" <span class="badge" style="color:var(--green);border-color:var(--green)">docs</span>"# } else { "" };
        rows.push_str(&format!(
            r#"<tr>
                 <td><a href="/registry/{ns}/{tp}">{ns}/<strong>{tp}</strong></a>{docs_badge}</td>
                 <td class="cyan">{latest}</td>
                 <td class="dim">{}</td>
                 <td class="dim">{}</td>
               </tr>"#,
            versions.len(),
            esc(&plats.join(", ")),
        ));
    }

    let table = if providers.is_empty() {
        r#"<div class="panel dim">No providers published yet.</div>"#.to_string()
    } else {
        format!(
            r#"<div class="panel" style="padding:0">
               <table>
                 <thead><tr><th>Provider</th><th>Latest</th><th>Versions</th><th>Platforms</th></tr></thead>
                 <tbody>{rows}</tbody>
               </table></div>"#
        )
    };

    let body = format!(
        r#"<h1>Provider Registry</h1>
           <p class="dim">Push: <code>POST /registry/providers/{{namespace}}/{{type}}/{{version}}/{{os}}/{{arch}}</code>
           &nbsp;·&nbsp; Mirror: <code>POST /registry/mirror</code></p>
           {table}"#
    );
    page("Registry", user.as_deref(), &body).into_response()
}

/// `GET /registry/{namespace}/{type}` — redirect to latest version docs.
pub async fn provider_page(
    State(app): State<AppState>,
    Path((ns, tp)): Path<(String, String)>,
    maybe: MaybeUser,
) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    let versions = app.registry.list_versions(&ns, &tp);
    if versions.is_empty() {
        return (StatusCode::NOT_FOUND, page("Not found", user.as_deref(),
            &format!(r#"<h1>{}/{}</h1><div class="panel dim">No such provider.</div>"#, esc(&ns), esc(&tp))
        )).into_response();
    }
    let latest = versions.last().unwrap();
    Redirect::to(&format!("/registry/{ns}/{tp}/{latest}/docs")).into_response()
}

/// Common inner renderer for all provider doc pages.
fn render_provider_doc_page(
    app: &AppState,
    ns: &str,
    tp: &str,
    ver: &str,
    doc_path: &str, // "index" | "resources/foo" | "data-sources/bar" | "guides/baz"
    user: Option<&str>,
) -> Response {
    let all_versions = app.registry.list_versions(ns, tp);
    if all_versions.is_empty() {
        return (StatusCode::NOT_FOUND, page("Not found", user,
            &format!(r#"<h1>{ns}/{tp}</h1><div class="panel dim">Provider not found.</div>"#)
        )).into_response();
    }

    // Build sidebar
    let resources    = app.registry.list_doc_category(ns, tp, ver, "resources");
    let data_sources = app.registry.list_doc_category(ns, tp, ver, "data-sources");
    let guides       = app.registry.list_doc_category(ns, tp, ver, "guides");

    let ver_opts: String = all_versions.iter().rev().map(|v| {
        format!(r#"<option value="{v}"{}>v{v}</option>"#,
            if v == ver { " selected" } else { "" })
    }).collect();

    let sidebar_link = |path: &str, label: &str| -> String {
        let href = if path == "index" {
            format!("/registry/{ns}/{tp}/{ver}/docs")
        } else {
            format!("/registry/{ns}/{tp}/{ver}/docs/{path}")
        };
        let cur = if path == doc_path { " cur" } else { "" };
        format!(r#"<div class="ds-link"><a href="{href}" class="{cur}">{}</a></div>"#, esc(label))
    };

    let mut sidebar = format!(
        r#"<div class="docs-sidebar">
        <div class="ds-provider"><a href="/registry/{ns}/{tp}">{ns}/<strong>{tp}</strong></a></div>
        <select onchange="window.location='/registry/{ns}/{tp}/'+this.value+'/docs'">{ver_opts}</select>
        {}"#,
        sidebar_link("index", "Overview"),
    );

    if !resources.is_empty() {
        sidebar.push_str(r#"<div class="ds-cat">Resources</div>"#);
        for r in &resources {
            sidebar.push_str(&sidebar_link(&format!("resources/{r}"), r));
        }
    }
    if !data_sources.is_empty() {
        sidebar.push_str(r#"<div class="ds-cat">Data Sources</div>"#);
        for ds in &data_sources {
            sidebar.push_str(&sidebar_link(&format!("data-sources/{ds}"), ds));
        }
    }
    if !guides.is_empty() {
        sidebar.push_str(r#"<div class="ds-cat">Guides</div>"#);
        for g in &guides {
            sidebar.push_str(&sidebar_link(&format!("guides/{g}"), g));
        }
    }
    sidebar.push_str("</div>");

    // Main content
    let md_content = if doc_path == "index" {
        app.registry.get_doc_file(ns, tp, ver, "index")
    } else {
        app.registry.get_doc_file(ns, tp, ver, doc_path)
    };

    let main_html = match md_content {
        Some(md) => {
            let body = strip_frontmatter(&md);
            format!(r#"<div class="docs-main md">{}</div>"#, render_markdown(body))
        }
        None if doc_path == "index" => {
            // No docs uploaded — show version table
            let mut rows = String::new();
            for v in all_versions.iter().rev() {
                let platforms = app.registry.list_platforms(ns, tp, v);
                let plats: Vec<String> = platforms.iter().map(|p| format!("{}_{}", p.os, p.arch)).collect();
                let meta = app.registry.get_meta(ns, tp, v).unwrap_or_default();
                let has_docs = app.registry.has_docs(ns, tp, v);
                let docs_link = if has_docs {
                    format!(r#" <a href="/registry/{ns}/{tp}/{v}/docs" class="dim" style="font-size:12px">docs</a>"#)
                } else { String::new() };
                rows.push_str(&format!(
                    r#"<tr><td class="cyan"><a href="/registry/{ns}/{tp}/{v}/docs">{v}</a>{docs_link}</td>
                       <td class="dim">{}</td><td class="dim">{}</td></tr>"#,
                    esc(&plats.join(", ")),
                    esc(&meta.protocols.join(", ")),
                ));
            }
            format!(
                r#"<div class="docs-main">
                <h1 style="font-size:22px;margin-bottom:6px"><span class="dim">{ns}/</span>{tp}</h1>
                <p class="dim" style="margin-bottom:24px">No documentation uploaded for this version.</p>
                <h2>Versions</h2>
                <div class="panel" style="padding:0"><table>
                  <thead><tr><th>Version</th><th>Platforms</th><th>Protocols</th></tr></thead>
                  <tbody>{rows}</tbody></table></div>
                <h2>Usage</h2>
                <div class="panel"><pre>terraform {{
  required_providers {{
    {tp} = {{
      source  = "&lt;registry-host&gt;/{ns}/{tp}"
      version = "{ver}"
    }}
  }}
}}</pre></div>
                <p class="dim">Upload docs: <code>PUT /registry/providers/{ns}/{tp}/{ver}/docs</code>
                   (ZIP from <code>terraform-plugin-docs</code> or plain Markdown)</p>
                </div>"#
            )
        }
        None => {
            format!(
                r#"<div class="docs-main"><div class="panel dim" style="margin-top:16px">
                Page not found: <code>{}</code></div></div>"#,
                esc(doc_path)
            )
        }
    };

    let title = if doc_path == "index" {
        format!("{ns}/{tp}")
    } else {
        format!("{ns}/{tp} — {}", doc_path.split('/').next_back().unwrap_or(doc_path))
    };

    let body = format!(r#"<div class="docs-layout">{sidebar}{main_html}</div>"#);
    page_docs(&title, user, &body).into_response()
}

/// `GET /registry/{namespace}/{type}/{version}/docs`
pub async fn provider_docs_index(
    State(app): State<AppState>,
    Path((ns, tp, ver)): Path<(String, String, String)>,
    maybe: MaybeUser,
) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    render_provider_doc_page(&app, &ns, &tp, &ver, "index", user.as_deref())
}

/// `GET /registry/{namespace}/{type}/{version}/docs/{*path}`
pub async fn provider_doc_page(
    State(app): State<AppState>,
    Path((ns, tp, ver, path)): Path<(String, String, String, String)>,
    maybe: MaybeUser,
) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    render_provider_doc_page(&app, &ns, &tp, &ver, &path, user.as_deref())
}

// ── Help page ────────────────────────────────────────────────────────────────

/// `GET /help` — public how-to page, no login required.
pub async fn help_page(maybe: MaybeUser) -> Html<String> {
    let user = maybe.user().map(|u| u.username.clone());

    // The JS snippet rewrites every `data-url` element with the actual origin
    // so all example snippets contain the real server address.
    let body = r#"
<h1>How to use terrarium</h1>
<p class="dim">Quick-start guide. The server address in all examples is filled in automatically.</p>

<script>
document.addEventListener("DOMContentLoaded", function() {
  var origin = window.location.origin;
  document.querySelectorAll("[data-url]").forEach(function(el) {
    el.textContent = el.textContent.replaceAll("https://terrarium.example", origin);
  });
});
</script>

<h2>1 — State backend (Terraform / OpenTofu)</h2>
<p class="dim">Store your Terraform state in terrarium instead of S3 or Terraform Cloud.</p>
<div class="panel"><pre data-url>terraform {
  backend "http" {
    address        = "https://terrarium.example/state/&lt;workspace&gt;"
    lock_address   = "https://terrarium.example/lock/&lt;workspace&gt;"
    unlock_address = "https://terrarium.example/lock/&lt;workspace&gt;"
    username       = "alice"
    password       = "your-password-or-api-token"
  }
}</pre></div>

<p class="dim">Replace <code>&lt;workspace&gt;</code> with any name you like — it can contain slashes, e.g. <code>infra/prod</code>.
Terrarium creates it automatically on first push. Use <a href="/tokens">API tokens</a> instead of your password when possible.</p>

<h2>2 — Provider registry</h2>
<p class="dim">Terrarium is a full Terraform provider registry. Push a provider binary, upload its docs, and source it directly from OpenTofu.</p>

<p><strong>Push a provider binary</strong> (one platform at a time):</p>
<div class="panel"><pre data-url>curl -u alice:token \
  -X POST "https://terrarium.example/registry/providers/&lt;namespace&gt;/&lt;type&gt;/&lt;version&gt;/&lt;os&gt;/&lt;arch&gt;" \
  --data-binary @terraform-provider-&lt;type&gt;_&lt;version&gt;_&lt;os&gt;_&lt;arch&gt;.zip</pre></div>

<p><strong>Upload documentation</strong> — the registry shows per-resource and per-data-source pages with a sidebar, just like registry.terraform.io.
Docs are sourced from the <code>docs/</code> directory that <a href="https://github.com/hashicorp/terraform-plugin-docs">terraform-plugin-docs</a> generates.</p>
<div class="panel"><pre data-url># Zip the provider's docs/ directory and upload it:
cd /path/to/terraform-provider-example
zip -r /tmp/example-docs.zip docs/
curl -u alice:token \
  -X PUT "https://terrarium.example/registry/providers/&lt;namespace&gt;/&lt;type&gt;/&lt;version&gt;/docs" \
  --data-binary @/tmp/example-docs.zip</pre></div>
<p class="dim">A plain Markdown body is also accepted and stored as the provider overview page.</p>

<p><strong>Source from this registry</strong> in your Terraform config:</p>
<div class="panel"><pre data-url>terraform {
  required_providers {
    example = {
      source  = "terrarium.example/&lt;namespace&gt;/&lt;type&gt;"
      version = "&lt;version&gt;"
    }
  }
}</pre></div>
<p class="dim">The service discovery endpoint (<code>/.well-known/terraform.json</code>) tells OpenTofu where to find the provider API automatically when the hostname matches.</p>

<h2>3 — Provider mirror</h2>
<p class="dim">Use terrarium as an OpenTofu network mirror. Providers are fetched lazily from the upstream registry named in the provider source, then cached locally. By default, <code>registry.terraform.io</code> and <code>registry.opentofu.org</code> are allowed.</p>

<p><strong>Configure OpenTofu to use terrarium as a network mirror</strong> (requires HTTPS):</p>
<div class="panel"><pre data-url># ~/.tofurc  (or TF_CLI_CONFIG_FILE)
provider_installation {
  network_mirror {
    url     = "https://terrarium.example/registry/mirror/"
    include = ["registry.opentofu.org/*/*", "registry.terraform.io/*/*"]
  }
  direct {
    exclude = ["registry.opentofu.org/*/*", "registry.terraform.io/*/*"]
  }
}</pre></div>

<p class="dim">On the first <code>tofu init</code>, terrarium discovers available versions from the upstream registry and downloads the requested provider archives on demand.</p>

<p><strong>Optional pre-warm via API:</strong></p>
<div class="panel"><pre data-url>curl -u alice:token \
  -X POST "https://terrarium.example/registry/mirror" \
  -H "Content-Type: application/json" \
  -d '{
    "namespace": "hashicorp",
    "type": "aws",
    "versions": ["5.60.0"],
    "platforms": [
      {"os": "linux",  "arch": "amd64"},
      {"os": "linux",  "arch": "arm64"},
      {"os": "darwin", "arch": "arm64"}
    ]
  }'</pre></div>
<p class="dim">Pre-warming is optional. Omit <code>versions</code> to mirror all versions. Omit <code>platforms</code> to use the default five (linux/darwin/windows × amd64/arm64).</p>

<p><strong>Optional automatic pre-warm on startup</strong> — place a <code>mirrors.json</code> file in the data directory (<code>TERRARIUM_DATA</code>):</p>
<div class="panel"><pre>[
  {
    "namespace": "hashicorp",
    "type": "aws",
    "versions": ["5.60.0"],
    "platforms": [{"os": "linux", "arch": "amd64"}, {"os": "darwin", "arch": "arm64"}]
  },
  {
    "namespace": "hashicorp",
    "type": "random"
  }
]</pre></div>
<p class="dim">Set <code>TERRARIUM_MIRROR_INTERVAL=86400</code> to refresh pre-warmed providers daily (value is in seconds). The NixOS module accepts this declaratively via <code>services.terrarium.mirrors</code> and <code>services.terrarium.mirrorInterval</code>.</p>

<p><strong>Filesystem mirror</strong> (works over plain HTTP, good for local dev):</p>
<div class="panel"><pre data-url># Download a provider zip from terrarium
mkdir -p /tmp/mirror/registry.terraform.io/hashicorp/null
curl -u alice:token \
  "https://terrarium.example/registry/providers/hashicorp/null/3.2.0/linux/amd64/zip" \
  -o /tmp/mirror/registry.terraform.io/hashicorp/null/terraform-provider-null_3.2.0_linux_amd64.zip

# ~/.tofurc
provider_installation {
  filesystem_mirror {
    path    = "/tmp/mirror"
    include = ["registry.terraform.io/hashicorp/null"]
  }
  direct {
    exclude = ["registry.terraform.io/hashicorp/null"]
  }
}</pre></div>

<h2>4 — terra CLI</h2>
<p class="dim">The <code>terra</code> binary wraps OpenTofu with a few extra terrarium-aware commands.</p>
<div class="panel"><pre>terra serve                          # start the server (reads TERRARIUM_DATA)
terra user add &lt;username&gt; &lt;pass&gt;   # create a user
terra user list                      # list users
terra user passwd &lt;username&gt;        # change password
terra login                          # save credentials to ~/.config/terrarium/config.toml

# All tofu commands pass through unchanged:
terra init / plan / apply / destroy / ...

terra remote state list              # list remote workspaces
terra remote state get &lt;name&gt;       # inspect a state
terra remote state versions &lt;name&gt;  # list saved versions
terra remote state diff &lt;name&gt; 1 2  # diff two versions
terra remote lock list               # show active locks
terra remote webhook add &lt;ws&gt; &lt;url&gt; # register a webhook</pre></div>

<h2>5 — Authentication</h2>
<p class="dim">All API endpoints (state, lock, registry upload) accept either:</p>
<ul style="padding-left:20px;line-height:2">
  <li><strong>HTTP Basic Auth</strong> — username + password (used by the Terraform backend directly)</li>
  <li><strong>Bearer token</strong> — create one on the <a href="/tokens">Tokens page</a>, pass as <code>Authorization: Bearer &lt;token&gt;</code></li>
</ul>
<p class="dim">Read-only registry endpoints (version list, download info, network mirror) are public — no auth needed.
The web UI uses a session cookie set at login.</p>

<h2>6 — Environment variables</h2>
<div class="panel"><pre>TERRARIUM_DATA             # server: data directory (default: current dir)
TERRARIUM_TLS              # server: set to 1 to mark session cookies Secure (use behind TLS)
TERRARIUM_MIRROR_INTERVAL  # server: refresh optional mirrors.json pre-warm every N seconds
TERRARIUM_UPSTREAM_REGISTRIES # server: comma-separated upstream registries allowed for lazy mirror
TERRARIUM_URL              # client: server URL
TERRARIUM_USER             # client: username
TERRARIUM_PASSWORD         # client: password
TERRARIUM_CONFIG           # client: path to config file</pre></div>
"#;

    page("Help", user.as_deref(), body)
}
