//! ctxd-dashboard — embedded web UI for the ctxd substrate.
//!
//! This crate ships the frontend (HTML/CSS/JS baked via rust-embed)
//! and the loopback-bypass middleware. The JSON API endpoints
//! (`/v1/events`, `/v1/subjects/tree`, `/v1/search`, etc.) live in
//! `ctxd-http` so non-browser HTTP clients (CLI scripts, ctxd-code,
//! future satellites) can use them too.
//!
//! ## Composition
//!
//! In `ctxd-cli/src/serve.rs` (step 6):
//!
//! ```ignore
//! let api = ctxd_http::router::build_router_with_hosts(
//!     store, cap_engine.clone(), caveat_state,
//!     ctxd_http::router::default_allowed_hosts(),
//! );
//! let frontend = ctxd_dashboard::router();
//! let app = api
//!     .merge(frontend)
//!     .layer(/* localhost_or_cap_token built from cap_engine */);
//! axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
//!     .await?;
//! ```
//!
//! The frontend router exposes `/` and `/static/{*path}` only; merging
//! it with the API router gives a single binding. The
//! `localhost_or_cap_token` layer wraps both, allowing browser callers
//! over loopback while still accepting cap-token clients from anywhere.

pub mod middleware;
pub mod static_assets;

use axum::routing::get;
use axum::Router;

/// Build the dashboard's frontend router. Routes:
///
/// - `GET /` → `index.html`
/// - `GET /static/{*path}` → individual asset
///
/// No middleware is applied here. Callers (the daemon's `serve.rs`)
/// compose this router with `ctxd-http`'s API router and wrap the
/// result with [`middleware::apply_localhost_or_cap_token`].
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(static_assets::serve_index))
        .route("/static/{*path}", get(static_assets::serve_asset))
}

pub use middleware::{apply_localhost_or_cap_token, LocalhostOrCapToken};
