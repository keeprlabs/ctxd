//! HTTP router and shared state for the ctxd admin API.
//!
//! Per-endpoint handlers live in `crate::handlers::*`. This file
//! defines [`AppState`] (shared across handlers) and [`build_router`]
//! (the canonical router builder).

use crate::handlers;
use crate::middleware::{apply_host_check, defensive_headers, DEFAULT_ALLOWED_HOSTS};
use axum::routing::{delete, get, post};
use axum::Router;
use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_store::EventStore;
use std::sync::Arc;
use std::time::Instant;
use tower_http::trace::TraceLayer;

/// Shared state for HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    /// The event store.
    pub store: EventStore,
    /// The capability engine.
    pub cap_engine: Arc<CapEngine>,
    /// Stateful-caveat backing store. Holds budgets and approvals.
    /// `Arc<dyn …>` so the daemon can swap impls (in-memory in tests,
    /// SQLite in `serve`) without changing handler code.
    pub caveat_state: Arc<dyn CaveatState>,
    /// Captured at router-construction time. `start_time.elapsed()`
    /// is the daemon's "uptime" surfaced via `GET /v1/stats`. There
    /// is no `std::process::start_time()`; we approximate by the
    /// router's first-build moment, which is close enough for an
    /// operator-facing counter.
    pub start_time: Instant,
}

/// Build the axum router with all admin endpoints.
///
/// **No host check**: equivalent to `build_router_with_hosts(..., vec![])`.
/// This is the test-friendly default — `tower::oneshot` and similar
/// helpers don't set a `Host:` header, so a default-on host check would
/// break every existing integration test. Production daemons that want
/// DNS-rebinding defense should call [`build_router_with_hosts`]
/// directly with a non-empty list (or pass `DEFAULT_ALLOWED_HOSTS`).
///
/// Defensive headers (CSP, X-Content-Type-Options, X-Frame-Options,
/// Referrer-Policy) are applied unconditionally either way.
pub fn build_router(
    store: EventStore,
    cap_engine: Arc<CapEngine>,
    caveat_state: Arc<dyn CaveatState>,
) -> Router {
    build_router_with_hosts(store, cap_engine, caveat_state, Vec::new())
}

/// Default v0.4 host allow-list: `127.0.0.1:7777`, `localhost:7777`,
/// `[::1]:7777`. Used when the daemon binds the standard port.
pub fn default_allowed_hosts() -> Vec<String> {
    DEFAULT_ALLOWED_HOSTS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Derive the allow-list from the daemon's actual bind address. Always
/// includes the bind address itself plus the matching `localhost:<port>`
/// and `[::1]:<port>` aliases. Use this instead of
/// [`default_allowed_hosts`] when the daemon binds a non-default port,
/// otherwise the dashboard 421s every fetch.
pub fn allowed_hosts_for_bind(addr: std::net::SocketAddr) -> Vec<String> {
    let port = addr.port();
    let mut hosts = vec![
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    // If the bind itself is a different IP (e.g. someone explicitly
    // binds 0.0.0.0 — discouraged but possible), include it too so
    // browsers using the bind hostname don't get rejected.
    let bind_str = addr.to_string();
    if !hosts.iter().any(|h| h == &bind_str) {
        hosts.push(bind_str);
    }
    hosts
}

/// Same as [`build_router`] but lets the caller override the
/// `Host:`-header allow-list (used in tests, and when the daemon binds
/// to a non-default port).
pub fn build_router_with_hosts(
    store: EventStore,
    cap_engine: Arc<CapEngine>,
    caveat_state: Arc<dyn CaveatState>,
    allowed_hosts: Vec<String>,
) -> Router {
    let state = AppState {
        store,
        cap_engine,
        caveat_state,
        start_time: Instant::now(),
    };
    let routes = Router::new()
        .route("/health", get(handlers::health::health))
        .route("/v1/grant", post(handlers::grants::grant))
        .route("/v1/stats", get(handlers::stats::stats))
        .route("/v1/approvals", get(handlers::approvals::list_approvals))
        .route(
            "/v1/approvals/{id}/decide",
            post(handlers::approvals::decide_approval),
        )
        .route("/v1/peers", get(handlers::peers::list_peers))
        .route("/v1/peers/{peer_id}", delete(handlers::peers::remove_peer))
        // v0.4 dashboard surface — read-only events / subjects / search,
        // SSE live tail, and the loopback-only hello-world tutorial write.
        .route("/v1/events", get(handlers::events::list_events))
        .route("/v1/events/stream", get(handlers::events::stream_events))
        .route("/v1/events/{id}", get(handlers::events::event_by_id))
        .route("/v1/subjects/tree", get(handlers::subjects::subject_tree))
        .route("/v1/search", get(handlers::search::search))
        .route(
            "/v1/dashboard/hello-world",
            post(handlers::dashboard::hello_world),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Compose middleware outermost-first: defensive headers wrap
    // everything (so host-check rejections also get them), then
    // host-check rejects bad Host before any handler runs.
    apply_host_check(routes, allowed_hosts).layer(axum::middleware::from_fn(defensive_headers))
}
