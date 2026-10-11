//! `GET /ui/search?q=` — results for the command palette.
//!
//! Fuzzy subsequence matching over workspaces, pages and providers.
//! `name@N` jumps straight to version N of a workspace.

use authur::MaybeUser;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::components::icon;
use super::data::Health;
use super::require_login;
use crate::AppState;

#[derive(serde::Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

struct Hit {
    href: String,
    label: String,
    positions: Vec<usize>,
    score: i32,
    icon: &'static str,
    hint: Markup,
    sans: bool,
}

const PAGES: &[(&str, &str, &str)] = &[
    ("Overview", "/", "overview"),
    ("Policies", "/policies", "policy"),
    ("Provider registry", "/registry", "registry"),
    ("API tokens & sessions", "/tokens", "key"),
    ("Help", "/help", "help"),
];

pub async fn results(maybe: MaybeUser, State(app): State<AppState>, Query(q): Query<SearchQuery>) -> Response {
    if maybe.user().is_none() {
        return require_login();
    }
    let query = q.q.trim();
    let (needle, version) = match query.rsplit_once('@') {
        Some((n, v)) if v.parse::<u32>().is_ok() => (n, v.parse::<u32>().ok()),
        _ => (query, None),
    };

    let mut workspaces: Vec<Hit> = Vec::new();
    for (archived, names) in [(false, app.state.list(None, false)), (true, app.state.list(None, true))] {
        for name in names {
            let Some((score, positions)) = fuzzy(&name, needle) else { continue };
            let health = Health::of(&app, &name, archived);
            let href = match version {
                Some(v) => format!("/graph/{name}?version={v}"),
                None => format!("/w/{name}"),
            };
            workspaces.push(Hit {
                hint: html! {
                    span.hint { @if let Some(v) = version { "v" (v) " · " } (health.label()) }
                },
                href,
                label: name,
                positions,
                score: score - if archived { 50 } else { 0 },
                icon: "folder",
                sans: false,
            });
        }
    }

    let mut pages: Vec<Hit> = PAGES
        .iter()
        .filter_map(|(label, href, ic)| {
            let (score, positions) = fuzzy(label, needle)?;
            Some(Hit { href: href.to_string(), label: label.to_string(), positions, score, icon: ic, hint: html! {}, sans: true })
        })
        .collect();

    let mut providers: Vec<Hit> = app
        .registry
        .list_providers()
        .into_iter()
        .filter_map(|(ns, tp)| {
            let label = format!("{ns}/{tp}");
            let (score, positions) = fuzzy(&label, needle)?;
            Some(Hit { href: format!("/registry/{ns}/{tp}"), label, positions, score, icon: "registry", hint: html! { span.hint { "provider" } }, sans: false })
        })
        .collect();

    for list in [&mut workspaces, &mut pages, &mut providers] {
        list.sort_by(|a, b| b.score.cmp(&a.score).then(a.label.cmp(&b.label)));
        list.truncate(8);
    }

    let mut first = true;
    let mut group = |title: &str, hits: &[Hit]| -> Markup {
        if hits.is_empty() {
            return html! {};
        }
        html! {
            div.grp { (title) }
            @for h in hits {
                a href=(h.href) role="option" aria-selected=(if std::mem::take(&mut first) { "true" } else { "false" }) {
                    (icon(h.icon))
                    span class={"label" @if h.sans { " sans" }} { (highlight(&h.label, &h.positions)) }
                    (h.hint)
                }
            }
        }
    };
    let body = html! {
        (group("Workspaces", &workspaces))
        (group("Pages", &pages))
        (group("Providers", &providers))
    };
    if workspaces.is_empty() && pages.is_empty() && providers.is_empty() {
        return html! { div.empty { "Nothing matches " b { (query) } "." } }.into_response();
    }
    body.into_response()
}

/// Case-insensitive fuzzy match. Contiguous matches beat scattered ones, and
/// earlier matches beat later ones. Returns the matched char positions.
fn fuzzy(hay: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    let h: Vec<char> = hay.to_lowercase().chars().collect();
    let n: Vec<char> = needle.to_lowercase().chars().collect();

    if let Some(start) = h.windows(n.len()).position(|w| w == n.as_slice()) {
        // Bonus for matching at a segment start (after `/`, `-`, `_`, `.`).
        let boundary = start == 0 || matches!(h[start - 1], '/' | '-' | '_' | '.' | ' ');
        let score = 1000 - start as i32 * 2 - h.len() as i32 + if boundary { 150 } else { 0 };
        return Some((score, (start..start + n.len()).collect()));
    }

    let mut positions = Vec::with_capacity(n.len());
    let mut i = 0;
    for c in &n {
        while i < h.len() && h[i] != *c {
            i += 1;
        }
        if i == h.len() {
            return None;
        }
        positions.push(i);
        i += 1;
    }
    let spread = (positions.last()? - positions[0]) as i32;
    Some((500 - spread * 4 - positions[0] as i32, positions))
}

fn highlight(label: &str, positions: &[usize]) -> Markup {
    html! {
        @for (i, c) in label.chars().enumerate() {
            @if positions.contains(&i) { mark { (c) } } @else { (c) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn substring_beats_scattered() {
        let (a, _) = fuzzy("infra/prod", "prod").unwrap();
        let (b, _) = fuzzy("p-r-o-d-x", "prod").unwrap();
        assert!(a > b);
    }

    #[test]
    fn subsequence_and_miss() {
        assert_eq!(fuzzy("infra/prod", "ipd").unwrap().1, vec![0, 6, 9]);
        assert!(fuzzy("infra/prod", "xyz").is_none());
    }
}
