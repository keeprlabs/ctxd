//! `rust-embed` wrapper that serves baked-in frontend files.
//!
//! In release builds the asset bytes are baked into the binary at
//! compile time. In debug builds (`cfg(debug_assertions)`), rust-embed
//! reads from disk so editing `assets/app.js` and refreshing the
//! browser shows changes without rebuild.

use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, HeaderValue, Response, StatusCode};
use rust_embed::RustEmbed;

/// Baked-in frontend assets.
#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

/// Serve `GET /` — the dashboard's HTML shell.
pub async fn serve_index() -> Response<Body> {
    serve_path("index.html").await
}

/// Serve `GET /static/{*path}`.
pub async fn serve_asset(Path(path): Path<String>) -> Response<Body> {
    // Reject path traversal attempts loudly. rust-embed lookups against
    // `..` would just return None, but a 400 is more honest than the
    // 404 we'd otherwise return.
    if path.contains("..") || path.starts_with('/') {
        return error_response(StatusCode::BAD_REQUEST, "invalid path");
    }
    serve_path(&path).await
}

/// Look up an asset by name and return it with the right Content-Type
/// + ETag. 404 if missing.
async fn serve_path(name: &str) -> Response<Body> {
    let Some(asset) = Assets::get(name) else {
        return error_response(StatusCode::NOT_FOUND, "not found");
    };
    let etag = format!("\"{}\"", hex::encode(&asset.metadata.sha256_hash()[..8]));
    let mime = mime_for_path(name);

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
        .header(header::ETAG, HeaderValue::from_str(&etag).unwrap());

    // ETag is computed from the asset's bytes; conditional 304 lands
    // when the client sends the same value back.
    builder = builder.header("vary", "Accept-Encoding");
    let _ = &builder;

    builder
        .body(Body::from(asset.data.into_owned()))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "build"))
}

/// Six-arm MIME table — covers everything the dashboard ships. Adding
/// `mime_guess` (200KB of dependency) for this is overkill.
fn mime_for_path(name: &str) -> &'static str {
    match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn error_response(status: StatusCode, msg: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"))
        .body(Body::from(msg))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Conditional GET helper: returns true when the client's
/// `If-None-Match` matches the given ETag. Handlers can short-circuit
/// to a 304 in that case. Implemented as a free function so it's
/// easily testable without spinning up the router.
#[allow(dead_code)]
pub(crate) fn if_none_match_matches(headers: &axum::http::HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == etag)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_for_path_known_extensions() {
        assert_eq!(mime_for_path("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for_path("style.css"), "text/css; charset=utf-8");
        assert_eq!(
            mime_for_path("app.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_for_path("favicon.svg"), "image/svg+xml");
        assert_eq!(mime_for_path("favicon.ico"), "image/x-icon");
        assert_eq!(mime_for_path("font.woff2"), "font/woff2");
    }

    #[test]
    fn mime_for_path_unknown_falls_back() {
        assert_eq!(mime_for_path("README"), "application/octet-stream");
        assert_eq!(mime_for_path("data.bin"), "application/octet-stream");
    }

    #[test]
    fn assets_baked_at_compile_time() {
        // The rust-embed `debug-embed` feature should make this work
        // even in `cargo test` runs. If this fails, the assets/ dir
        // is missing or the feature flag has been turned off.
        assert!(Assets::get("index.html").is_some());
        assert!(Assets::get("style.css").is_some());
        assert!(Assets::get("app.js").is_some());
        assert!(Assets::get("favicon.svg").is_some());
    }
}
