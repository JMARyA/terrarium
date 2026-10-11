//! Static assets compiled into the binary.
//!
//! Pages reference assets as `/static/{file}?v={hash}`; the hash changes with
//! the content, so responses can be cached forever without serving a stale
//! stylesheet after an upgrade.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

struct Asset {
    name: &'static str,
    mime: &'static str,
    body: &'static [u8],
}

const ASSETS: &[Asset] = &[
    Asset { name: "app.css", mime: "text/css; charset=utf-8", body: include_bytes!("static/app.css") },
    Asset { name: "app.js", mime: "text/javascript; charset=utf-8", body: include_bytes!("static/app.js") },
    Asset { name: "htmx.min.js", mime: "text/javascript; charset=utf-8", body: include_bytes!("static/htmx.min.js") },
    Asset { name: "hanken-grotesk.woff2", mime: "font/woff2", body: include_bytes!("static/hanken-grotesk.woff2") },
    Asset { name: "jetbrains-mono.woff2", mime: "font/woff2", body: include_bytes!("static/jetbrains-mono.woff2") },
];

/// Short content hash per asset, computed once.
static VERSIONS: LazyLock<Vec<String>> = LazyLock::new(|| {
    ASSETS
        .iter()
        .map(|a| {
            let digest = Sha256::digest(a.body);
            digest[..6].iter().map(|b| format!("{b:02x}")).collect()
        })
        .collect()
});

/// URL for an asset, with its cache-busting hash.
pub fn url(name: &str) -> String {
    match ASSETS.iter().position(|a| a.name == name) {
        Some(i) => format!("/static/{name}?v={}", VERSIONS[i]),
        None => format!("/static/{name}"),
    }
}

pub async fn serve(Path(file): Path<String>) -> Response {
    let Some(asset) = ASSETS.iter().find(|a| a.name == file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, asset.mime),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        asset.body,
    )
        .into_response()
}
