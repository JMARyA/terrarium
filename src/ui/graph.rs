//! Resource dependency graph as inline SVG.
//!
//! Nodes are resource addresses; edges run from a dependency to the resource
//! that depends on it. Layout is longest-path layering — depended-upon
//! resources sit to the left, dependents to the right. Hovering or clicking a
//! node highlights its neighbours (`app.js`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use maud::{Markup, html};

use crate::tfstate::TfState;

const COL_W: usize = 290;
const ROW_H: usize = 62;
const BOX_W: usize = 236;
const BOX_H: usize = 44;
const PAD: usize = 24;

pub fn render(state: &TfState) -> Option<Markup> {
    let mut modes: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &state.resources {
        let addr = r.address();
        let label = match &r.module {
            Some(m) => format!("{m}.{}", r.name),
            None => r.name.clone(),
        };
        modes.insert(addr.clone(), (r.mode.clone(), r.type_.clone(), label));
        let entry = deps.entry(addr).or_default();
        for inst in &r.instances {
            entry.extend(inst.dependencies.iter().flatten().cloned());
        }
    }
    if modes.is_empty() {
        return None;
    }
    // Keep only edges that point at resources present in this state.
    for set in deps.values_mut() {
        set.retain(|d| modes.contains_key(d));
    }

    fn depth(
        addr: &str,
        deps: &BTreeMap<String, BTreeSet<String>>,
        memo: &mut HashMap<String, usize>,
        stack: &mut BTreeSet<String>,
    ) -> usize {
        if let Some(d) = memo.get(addr) {
            return *d;
        }
        if !stack.insert(addr.to_string()) {
            return 0; // break dependency cycle
        }
        let d = deps
            .get(addr)
            .and_then(|s| s.iter().map(|dep| depth(dep, deps, memo, stack) + 1).max())
            .unwrap_or(0);
        stack.remove(addr);
        memo.insert(addr.to_string(), d);
        d
    }

    let mut memo = HashMap::new();
    let mut layers: BTreeMap<usize, Vec<&String>> = BTreeMap::new();
    for addr in modes.keys() {
        let l = depth(addr, &deps, &mut memo, &mut BTreeSet::new());
        layers.entry(l).or_default().push(addr);
    }

    let mut pos: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut max_rows = 0usize;
    for (layer, members) in &layers {
        max_rows = max_rows.max(members.len());
        for (row, addr) in members.iter().enumerate() {
            pos.insert(addr.as_str(), (PAD + layer * COL_W, PAD + row * ROW_H));
        }
    }
    let width = PAD * 2 + (layers.len().max(1) - 1) * COL_W + BOX_W;
    let height = PAD * 2 + (max_rows.max(1) - 1) * ROW_H + BOX_H;

    Some(html! {
        svg.graph viewBox={"0 0 " (width) " " (height)} width=(width) height=(height)
            xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Resource dependency graph" {
            defs {
                marker #arrow viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse" {
                    path d="M0,0 L10,5 L0,10 z" fill="#4c6656" {}
                }
            }
            // Edges first, so nodes draw on top.
            @for (addr, set) in &deps {
                @if let Some(&(nx, ny)) = pos.get(addr.as_str()) {
                    @for dep in set {
                        @if let Some(&(dx, dy)) = pos.get(dep.as_str()) {
                            @let (x1, y1) = (dx + BOX_W, dy + BOX_H / 2);
                            @let (x2, y2) = (nx.saturating_sub(4), ny + BOX_H / 2);
                            @let mid = (x1 + x2) / 2;
                            path.edge data-from=(dep) data-to=(addr) marker-end="url(#arrow)"
                                d={"M" (x1) "," (y1) " C" (mid) "," (y1) " " (mid) "," (y2) " " (x2) "," (y2)} {}
                        }
                    }
                }
            }
            @for (addr, (mode, ty, label)) in &modes {
                @let (x, y) = pos[addr.as_str()];
                g class={"node " (if mode == "data" { "data" } else { "managed" })} data-id=(addr) {
                    title { (addr) }
                    rect x=(x) y=(y) width=(BOX_W) height=(BOX_H) rx="8" {}
                    text.t x=(x + 12) y=(y + 17) { (truncate(&if mode == "data" { format!("data.{ty}") } else { ty.clone() }, 34)) }
                    text.nm x=(x + 12) y=(y + 33) { (truncate(label, 28)) }
                }
            }
        }
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let t: String = s.chars().take(n - 1).collect();
        format!("{t}…")
    } else {
        s.to_string()
    }
}
