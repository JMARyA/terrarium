//! Small reusable pieces of markup.

use chrono::{DateTime, Utc};
use maud::{Markup, PreEscaped, html};

/// Inline stroke icon (24×24 viewBox, sized by CSS).
pub fn icon(name: &str) -> Markup {
    let paths = match name {
        "overview" => r#"<rect x="3.5" y="3.5" width="7" height="7" rx="1.5"/><rect x="13.5" y="3.5" width="7" height="4.5" rx="1.5"/><rect x="13.5" y="11" width="7" height="9.5" rx="1.5"/><rect x="3.5" y="13.5" width="7" height="7" rx="1.5"/>"#,
        "policy" => r#"<path d="M12 3 4.5 6v5.5c0 4.6 3.1 8.2 7.5 9.5 4.4-1.3 7.5-4.9 7.5-9.5V6z"/><path d="m9 12 2.2 2.2L15.5 10"/>"#,
        "registry" => r#"<path d="M3.5 7.5 12 3l8.5 4.5v9L12 21l-8.5-4.5z"/><path d="M3.5 7.5 12 12l8.5-4.5M12 12v9"/>"#,
        "key" => r#"<circle cx="8" cy="15" r="4"/><path d="m11 12 8.5-8.5M16 7l2.5 2.5M14 9l2 2"/>"#,
        "help" => r#"<circle cx="12" cy="12" r="9"/><path d="M9.5 9.3a2.6 2.6 0 0 1 5 .9c0 1.8-2.5 2.3-2.5 3.8M12 17h.01"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#,
        "lock" => r#"<rect x="5" y="10.5" width="14" height="10" rx="2.5"/><path d="M8 10.5V8a4 4 0 0 1 8 0v2.5"/>"#,
        "unlock" => r#"<rect x="5" y="10.5" width="14" height="10" rx="2.5"/><path d="M8 10.5V8a4 4 0 0 1 7.7-1.5"/>"#,
        "archive" => r#"<rect x="3.5" y="4" width="17" height="4.5" rx="1.2"/><path d="M5 8.5v9.5a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5M10 12.5h4"/>"#,
        "logout" => r#"<path d="M14 4h3.5a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H14M10 16l-4-4 4-4M6 12h10"/>"#,
        "chevron" => r#"<path d="m9 6 6 6-6 6"/>"#,
        "copy" => r#"<rect x="8.5" y="8.5" width="12" height="12" rx="2.5"/><path d="M15.5 8.5V6a2.5 2.5 0 0 0-2.5-2.5H6A2.5 2.5 0 0 0 3.5 6v7A2.5 2.5 0 0 0 6 15.5h2.5"/>"#,
        "graph" => r#"<circle cx="6" cy="6" r="2.5"/><circle cx="6" cy="18" r="2.5"/><circle cx="18" cy="12" r="2.5"/><path d="M8.5 6.5c4 .5 5.5 3 7 4.5M8.5 17.5c4-.5 5.5-3 7-4.5"/>"#,
        "history" => r#"<path d="M3.5 12a8.5 8.5 0 1 0 2.6-6.1"/><path d="M3.5 4v4.5H8M12 7.5V12l3 2"/>"#,
        "layers" => r#"<path d="m12 3.5 8.5 4.5-8.5 4.5L3.5 8z"/><path d="m3.5 12 8.5 4.5 8.5-4.5M3.5 16l8.5 4.5 8.5-4.5"/>"#,
        "push" => r#"<path d="M12 19V6M6.5 11.5 12 6l5.5 5.5"/>"#,
        "webhook" => r#"<path d="M9.5 8.5a3.5 3.5 0 1 1 5.2 3L17 15.5"/><path d="M7 14.5a3.5 3.5 0 1 0 4 4.5h5"/><path d="M15.5 13.5a3.5 3.5 0 1 1-1.7 6.5"/>"#,
        "check" => r#"<path d="m5 12.5 4.5 4.5L19 7.5"/>"#,
        "alert" => r#"<path d="M12 4 2.8 19.5h18.4z"/><path d="M12 10v4.5M12 17.5h.01"/>"#,
        "sprout" => r#"<path d="M12 21v-8"/><path d="M12 13c-3.6 0-6-2.2-6-5.6 3.6 0 6 2.2 6 5.6z"/><path d="M12 11c0-3.2 2.3-5.5 6-5.5 0 3.2-2.3 5.5-6 5.5z"/>"#,
        "menu" => r#"<path d="M4 7h16M4 12h16M4 17h16"/>"#,
        "file" => r#"<path d="M14 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-10z"/><path d="M14 3.5v5h5"/>"#,
        "arrow" => r#"<path d="M5 12h14M13 6l6 6-6 6"/>"#,
        "folder" => r#"<path d="M3.5 7a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>"#,
        _ => "",
    };
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { (PreEscaped(paths)) }
    }
}

/// The terrarium mark: a glass dome over a sprout.
pub fn logo() -> Markup {
    html! {
        svg viewBox="0 0 32 32" fill="none" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" {
            path d="M7 14a9 9 0 0 1 18 0v10.5a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 7 24.5z" {}
            path d="M7 22h18" opacity="0.55" {}
            path d="M16 22v-6" {}
            path d="M16 17.5c-2.6 0-4.3-1.6-4.3-4 2.6 0 4.3 1.6 4.3 4z" fill="currentColor" fill-opacity="0.25" {}
            path d="M16 16c0-2.4 1.7-4 4.3-4 0 2.4-1.7 4-4.3 4z" fill="currentColor" fill-opacity="0.25" {}
            path d="M11 11.5a6 6 0 0 1 3-3" opacity="0.5" {}
        }
    }
}

/// `OperationTypeApply` → `apply`.
pub fn op_label(op: &str) -> String {
    op.trim_start_matches("OperationType").to_lowercase()
}

pub fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn rel_label(at: DateTime<Utc>) -> String {
    let secs = (Utc::now() - at).num_seconds();
    match secs {
        ..45 => "just now".into(),
        45..3_600 => format!("{}m ago", (secs + 30) / 60),
        3_600..86_400 => format!("{}h ago", (secs + 1_800) / 3_600),
        86_400..2_592_000 => format!("{}d ago", (secs + 43_200) / 86_400),
        _ => at.format("%b %-d, %Y").to_string(),
    }
}

/// A `<time>` that reads relative ("3m ago") and shows the exact UTC moment
/// on hover. `app.js` keeps the label current.
pub fn time(at: DateTime<Utc>) -> Markup {
    html! {
        time datetime=(at.to_rfc3339()) title=(at.format("%Y-%m-%d %H:%M:%S UTC")) data-rel { (rel_label(at)) }
    }
}

pub fn opt_time(at: Option<DateTime<Utc>>) -> Markup {
    match at {
        Some(at) => time(at),
        None => html! { span.faint { "—" } },
    }
}

pub fn empty(icon_name: &str, text: Markup) -> Markup {
    html! { div.empty { (icon(icon_name)) div { (text) } } }
}

/// Signed size change between two versions.
pub fn delta(size: u64, prev: Option<u64>) -> Markup {
    let Some(prev) = prev else {
        return html! { span.delta.flat { "new" } };
    };
    let d = size as i64 - prev as i64;
    if d == 0 {
        return html! { span.delta.flat { "±0" } };
    }
    let (class, sign) = if d > 0 { ("up", "+") } else { ("down", "−") };
    html! { span class={"delta " (class)} { (sign) (fmt_size(d.unsigned_abs())) } }
}

/// The namespace path above a workspace, as a breadcrumb.
pub fn crumbs(name: &str) -> Markup {
    let segs: Vec<&str> = name.split('/').collect();
    html! {
        div.crumbs {
            a href="/" { "workspaces" }
            @for seg in &segs[..segs.len() - 1] {
                span.sep { "/" }
                span { (seg) }
            }
        }
    }
}

/// Tiny inline bar sparkline.
pub fn spark(values: &[u32]) -> Markup {
    let max = values.iter().copied().max().unwrap_or(0).max(1) as f64;
    let n = values.len().max(1) as f64;
    let w = 92.0;
    let h = 30.0;
    let bw = w / n;
    let mut bars = String::new();
    for (i, v) in values.iter().enumerate() {
        let bh = ((*v as f64 / max) * (h - 2.0)).max(if *v > 0 { 2.0 } else { 1.0 });
        let op = if i + 1 == values.len() { 1.0 } else { 0.55 };
        bars.push_str(&format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="1" fill="{}" opacity="{op}"/>"#,
            i as f64 * bw + 0.75,
            h - bh,
            (bw - 1.5).max(1.0),
            bh,
            if *v > 0 { "var(--moss)" } else { "var(--line-2)" },
        ));
    }
    html! {
        svg.spark viewBox={"0 0 " (w) " " (h)} preserveAspectRatio="none" aria-hidden="true" { (PreEscaped(bars)) }
    }
}

pub fn csrf_input(token: &str) -> Markup {
    html! { input type="hidden" name="csrf" value=(token); }
}

pub fn codeblock(code: &str) -> Markup {
    html! {
        div.codeblock {
            pre { (code) }
            button.btn.sm.copy type="button" data-copy="" aria-label="Copy to clipboard" {
                (icon("copy")) span.label { "Copy" }
            }
        }
    }
}
