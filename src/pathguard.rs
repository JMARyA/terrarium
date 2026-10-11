//! Request-path hygiene, applied to every route.
//!
//! Handlers build filesystem paths from URL segments (workspace names,
//! provider namespace/type/version, doc paths). Rather than trust each one to
//! validate, requests whose path could step outside the segment it names are
//! refused up front: a `.` or `..` segment, or a segment that decodes to
//! something containing `/`, `\` or NUL (`%2F`, `%5C`, `%00`). Joining such a
//! value onto a base directory either climbs out of it or, when it starts with
//! `/`, replaces the base entirely.

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub async fn reject_unsafe_paths(req: Request, next: Next) -> Response {
    if !is_safe_path(req.uri().path()) {
        tracing::warn!("🚫 Rejected unsafe request path {:?}", req.uri().path());
        return StatusCode::BAD_REQUEST.into_response();
    }
    next.run(req).await
}

pub fn is_safe_path(path: &str) -> bool {
    path.split('/').all(|raw| {
        let Some(seg) = percent_decode(raw) else { return false };
        seg != "." && seg != ".." && !seg.contains(['/', '\\', '\0'])
    })
}

/// Whether `seg` is usable as a single path component.
pub fn is_safe_segment(seg: &str) -> bool {
    !seg.is_empty() && seg != "." && seg != ".." && !seg.contains(['/', '\\', '\0'])
}

/// Decode `%XX` escapes. `None` for malformed escapes or non-UTF-8 results,
/// which are refused rather than guessed at.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_paths() {
        assert!(is_safe_path("/"));
        assert!(is_safe_path("/state/infra/prod"));
        assert!(is_safe_path("/registry/hashicorp/aws/5.60.0/docs/resources/instance"));
        assert!(is_safe_path("/registry/mirror/registry.terraform.io/hashicorp/aws/index.json"));
        assert!(is_safe_path("/w/apps/my%20app"));
    }

    #[test]
    fn rejects_traversal() {
        assert!(!is_safe_path("/registry/a/b/1/docs/../../../users/users/alice"));
        assert!(!is_safe_path("/registry/a/b/1/docs/%2e%2e/%2E%2E/etc/passwd"));
        assert!(!is_safe_path("/registry/%2Fetc/b/versions"));
        assert!(!is_safe_path("/state/a%5C..%5Cb"));
        assert!(!is_safe_path("/state/a%00"));
        assert!(!is_safe_path("/state/./x"));
        assert!(!is_safe_path("/state/%zz"));
    }
}
