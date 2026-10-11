//! `GET /ui/resource/{name}?version=N&addr=A` — one resource's attributes,
//! loaded into the State tab's inspector.
//!
//! Values are masked when Terraform marks them sensitive, and also when their
//! key looks like a secret: states routinely hold credentials that nobody
//! flagged. Masked values never leave the server.

use authur::MaybeUser;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};
use serde_json::Value;

use super::components::{empty, icon};
use super::{require_login, valid_name};
use crate::AppState;

#[derive(serde::Deserialize)]
pub struct InspectQuery {
    version: Option<u32>,
    addr: String,
}

/// Key fragments whose string values are masked even when not flagged.
const SECRET_KEYS: &[&str] = &[
    "password", "passwd", "secret", "token", "private_key", "privatekey", "access_key", "api_key", "apikey",
    "credential", "connection_string", "client_key",
];

/// One step into a value: an object key or an array index.
#[derive(Clone, PartialEq, Debug)]
enum Step {
    Key(String),
    Index(u64),
}

pub async fn fragment(
    maybe: MaybeUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<InspectQuery>,
) -> Response {
    if maybe.user().is_none() {
        return require_login();
    }
    if !valid_name(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let blob = match q.version {
        Some(v) => app.state.get_version(&name, v),
        None => app.state.get(&name),
    };
    let state: Option<Value> = blob.ok().flatten().and_then(|b| serde_json::from_slice(&b).ok());
    let resource = state
        .as_ref()
        .and_then(|s| s["resources"].as_array())
        .and_then(|rs| rs.iter().find(|r| address(r).as_deref() == Some(q.addr.as_str())));

    let Some(resource) = resource else {
        return card(&q.addr, empty("alert", html! { "Not in this version." })).into_response();
    };
    let instances = resource["instances"].as_array().cloned().unwrap_or_default();
    let body = html! {
        @if instances.is_empty() {
            (empty("layers", html! { "No instances." }))
        }
        @for inst in &instances {
            @let sensitive = sensitive_paths(&inst["sensitive_attributes"]);
            @let deps = inst["dependencies"].as_array().map(|d| d.len()).unwrap_or(0);
            div.inst {
                @if instances.len() > 1 || !inst["index_key"].is_null() {
                    div.inst-h {
                        span.mono { "[" (scalar(&inst["index_key"])) "]" }
                    }
                }
                @match &inst["attributes"] {
                    Value::Object(map) if !map.is_empty() => (tree(&inst["attributes"], &mut Vec::new(), &sensitive, false)),
                    _ => div.card-b.muted { "No attributes recorded." },
                }
                @if deps > 0 {
                    div.inst-f.small.faint { "depends on " (deps) " resource" (super::data::plural(deps)) }
                }
            }
        }
    };
    card(&q.addr, body).into_response()
}

fn card(addr: &str, body: Markup) -> Markup {
    html! {
        section.card.inspector {
            div.card-h {
                h2 { (icon("file")) span.mono style="overflow-wrap:anywhere" { (addr) } }
                span.aside {
                    button.btn.quiet.sm type="button" aria-label="Close inspector"
                        onclick="this.closest('#inspector').innerHTML = document.getElementById('inspector-empty').innerHTML" { "✕" }
                }
            }
            (body)
        }
    }
}

/// The address Terraform would print for a resource object in state JSON.
fn address(r: &Value) -> Option<String> {
    let ty = r["type"].as_str()?;
    let name = r["name"].as_str()?;
    let base = if r["mode"].as_str() == Some("data") { format!("data.{ty}.{name}") } else { format!("{ty}.{name}") };
    Some(match r["module"].as_str() {
        Some(m) => format!("{m}.{base}"),
        None => base,
    })
}

/// Paths Terraform marks sensitive. Older states write each path as a list
/// of steps (`[[{"type":"get_attr","value":"password"}]]`); some write one
/// step per entry. Both are understood.
fn sensitive_paths(v: &Value) -> Vec<Vec<Step>> {
    let step = |s: &Value| -> Option<Step> {
        match s["type"].as_str()? {
            "get_attr" => Some(Step::Key(s["value"].as_str()?.to_string())),
            "index" => match &s["value"]["value"] {
                Value::Number(n) => n.as_u64().map(Step::Index),
                Value::String(k) => Some(Step::Key(k.clone())),
                _ => None,
            },
            _ => None,
        }
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry {
            Value::Array(steps) => steps.iter().map(step).collect(),
            Value::Object(_) => step(entry).map(|s| vec![s]),
            _ => None,
        })
        .collect()
}

fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|s| k.contains(s))
}

fn masked(path: &[Step], sensitive: &[Vec<Step>], inherited: bool) -> bool {
    inherited
        || sensitive.iter().any(|p| path.starts_with(p))
        || matches!(path.last(), Some(Step::Key(k)) if looks_secret(k))
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{s:?}"),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

fn tree(v: &Value, path: &mut Vec<Step>, sensitive: &[Vec<Step>], hidden: bool) -> Markup {
    let hidden = masked(path, sensitive, hidden);
    match v {
        _ if hidden && !v.is_null() && !matches!(v, Value::Object(_) | Value::Array(_)) => html! {
            span.v.masked title="sensitive — not shown" { "••••••" }
        },
        Value::Object(map) if map.is_empty() => html! { span.v.faint { "{}" } },
        Value::Array(items) if items.is_empty() => html! { span.v.faint { "[]" } },
        Value::Object(map) => html! {
            dl.attrs {
                @for (k, child) in map {
                    @let _ = path.push(Step::Key(k.clone()));
                    dt { (k) }
                    dd { (tree(child, path, sensitive, hidden)) }
                    @let _ = path.pop();
                }
            }
        },
        Value::Array(items) => html! {
            ol.attrs-list {
                @for (i, child) in items.iter().enumerate() {
                    @let _ = path.push(Step::Index(i as u64));
                    li { (tree(child, path, sensitive, hidden)) }
                    @let _ = path.pop();
                }
            }
        },
        Value::String(s) if s.chars().count() > 240 => html! {
            details.v.long {
                summary { span.str { (s.chars().take(80).collect::<String>()) "…" } span.faint { " " (s.chars().count()) " chars" } }
                span.str { (s) }
            }
        },
        Value::String(s) => html! { span.v.str { (s) } },
        Value::Number(n) => html! { span.v.num { (n.to_string()) } },
        Value::Bool(b) => html! { span.v.bool { (b) } },
        Value::Null => html! { span.v.faint { "null" } },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render(attrs: Value, sensitive: Value) -> String {
        tree(&attrs, &mut Vec::new(), &sensitive_paths(&sensitive), false).0
    }

    #[test]
    fn flagged_and_secret_looking_values_are_masked() {
        let html = render(
            json!({"id": "i-1", "master_password": "hunter2", "config": {"endpoint": "db.example", "auth": "s3cret"}, "tags": ["a"]}),
            json!([[{"type": "get_attr", "value": "config"}, {"type": "get_attr", "value": "auth"}]]),
        );
        assert!(html.contains("i-1") && html.contains("db.example"), "{html}");
        assert!(!html.contains("hunter2"), "secret-looking key must be masked: {html}");
        assert!(!html.contains("s3cret"), "flagged path must be masked: {html}");
    }

    #[test]
    fn single_step_sensitive_format_and_whole_subtrees() {
        let html = render(json!({"keys": {"a": "x1", "b": "x2"}, "name": "ok"}), json!([{"type": "get_attr", "value": "keys"}]));
        assert!(!html.contains("x1") && !html.contains("x2"), "{html}");
        assert!(html.contains("ok"));
    }

    #[test]
    fn addresses_match_terraform() {
        assert_eq!(address(&json!({"mode":"managed","type":"aws_vpc","name":"main"})).unwrap(), "aws_vpc.main");
        assert_eq!(address(&json!({"mode":"data","type":"aws_ami","name":"u","module":"module.x"})).unwrap(), "module.x.data.aws_ami.u");
    }
}
