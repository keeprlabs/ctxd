//! `GET /health` — daemon liveness check.

use axum::response::IntoResponse;
use axum::Json;

/// Always returns `{"status":"ok","version":"<crate version>"}`. The
/// version reflects `ctxd-http`'s `Cargo.toml`, which is workspace-
/// versioned with the rest of ctxd.
pub(crate) async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
