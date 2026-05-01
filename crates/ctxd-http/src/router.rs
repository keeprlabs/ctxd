//! HTTP router and shared state for the ctxd admin API.
//!
//! Per-endpoint handlers live in `crate::handlers::*`. This file
//! defines [`AppState`] (shared across handlers) and [`build_router`]
//! (the canonical router builder).

use crate::handlers;
use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::trace::TraceLayer;
use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_store::EventStore;
use std::sync::Arc;
use std::time::Instant;

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
pub fn build_router(
    store: EventStore,
    cap_engine: Arc<CapEngine>,
    caveat_state: Arc<dyn CaveatState>,
) -> Router {
    let state = AppState {
        store,
        cap_engine,
        caveat_state,
        start_time: Instant::now(),
    };
    Router::new()
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
        .with_state(state)
}
