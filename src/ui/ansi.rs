//! Turn the ANSI-coloured attribute diff from `rediff` into HTML.
//!
//! `rediff` colours with 24-bit SGR codes from a fixed palette: a muted blue
//! for structure and unchanged items, teal for additions and keys, pink-red for
//! removals. Those map onto theme classes instead of being thrown away, so
//! the web diff reads like the terminal one.

use maud::{Markup, PreEscaped};

pub fn to_html(input: &str) -> Markup {
    let mut out = String::with_capacity(input.len() + input.len() / 4);
    let mut open = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            let mut params = String::new();
            let mut fin = None;
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    fin = Some(n);
                    break;
                }
                params.push(n);
            }
            if fin != Some('m') {
                continue; // not a colour code; drop it
            }
            if open {
                out.push_str("</span>");
                open = false;
            }
            if let Some(class) = class_for(&params) {
                out.push_str("<span class=\"");
                out.push_str(class);
                out.push_str("\">");
                open = true;
            }
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    if open {
        out.push_str("</span>");
    }
    PreEscaped(out)
}

fn class_for(params: &str) -> Option<&'static str> {
    let p: Vec<&str> = params.split(';').collect();
    match p.as_slice() {
        ["38", "2", r, g, b] => {
            let (r, g, b): (u16, u16, u16) = (r.parse().ok()?, g.parse().ok()?, b.parse().ok()?);
            if r > g + 60 && r > b {
                Some("a-del")
            } else if g > r + 60 {
                Some("a-add")
            } else {
                Some("a-dim")
            }
        }
        ["31"] | ["91"] => Some("a-del"),
        ["32"] | ["92"] | ["36"] | ["96"] => Some("a-add"),
        ["2"] | ["90"] => Some("a-dim"),
        _ => None, // reset (0, 39, empty) or unhandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_rediff_palette_and_escapes() {
        let s = "\u{1b}[38;2;86;95;137m{\u{1b}[39m \u{1b}[38;2;247;118;142m\"<old>\"\u{1b}[39m → \u{1b}[38;2;115;218;202m\"new\"\u{1b}[39m";
        assert_eq!(
            to_html(s).0,
            r#"<span class="a-dim">{</span> <span class="a-del">&quot;&lt;old&gt;&quot;</span> → <span class="a-add">&quot;new&quot;</span>"#
        );
    }
}
