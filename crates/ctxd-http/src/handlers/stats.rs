//! `GET /v1/stats` — substrate counters + uptime. Used as the dashboard
//! overview's headline numbers and as a readiness probe.

use crate::router::AppState;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;

/// Substrate counters and identity. Extended in v0.4 from the v0.3
/// `{ subject_count }` shape; new fields are additive so older clients
/// continue to read what they expected.
pub(crate) async fn stats(State(state): State<AppState>) -> impl IntoResponse {
    // Best-effort everywhere: the dashboard would rather render `0`
    // than 500 if a single counter call hiccups (e.g. WAL contention
    // mid-write). Errors are logged for ops, not surfaced to UI.
    let event_count = state.store.event_count().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "stats: event_count failed");
        0
    });
    let vector_embedding_count = state
        .store
        .vector_embedding_count()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "stats: vector_embedding_count failed");
            0
        });
    let subjects = state.store.subjects(None, false).await.unwrap_or_default();
    let peers = state.store.peer_list_impl().await.unwrap_or_default();
    let pending = state
        .store
        .pending_approvals_list()
        .await
        .unwrap_or_default();
    let uptime_seconds = state.start_time.elapsed().as_secs();

    Json(serde_json::json!({
        "event_count": event_count,
        "subject_count": subjects.len(),
        "peer_count": peers.len(),
        "pending_approval_count": pending.len(),
        "vector_embedding_count": vector_embedding_count,
        "uptime_seconds": uptime_seconds,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}
