//! HTTP router and handlers for the ctxd admin API.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use ctxd_cap::{CapEngine, Operation};
use ctxd_store::EventStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Shared state for HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    /// The event store.
    pub store: EventStore,
    /// The capability engine.
    pub cap_engine: Arc<CapEngine>,
}

/// Build the axum router with all admin endpoints.
pub fn build_router(store: EventStore, cap_engine: Arc<CapEngine>) -> Router {
    let state = AppState { store, cap_engine };
    Router::new()
        .route("/health", get(health))
        .route("/v1/grant", post(grant))
        .route("/v1/stats", get(stats))
        .with_state(state)
}

/// Health check endpoint.
async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Request body for the grant endpoint.
#[derive(Debug, Deserialize)]
struct GrantRequest {
    /// Subject glob pattern the token grants access to.
    subject: String,
    /// Operations to grant.
    operations: Vec<String>,
    /// Optional expiry in seconds from now.
    expires_in_secs: Option<i64>,
}

/// Response from the grant endpoint.
#[derive(Debug, Serialize)]
struct GrantResponse {
    /// Base64-encoded capability token.
    token: String,
}

/// Mint a new capability token.
async fn grant(
    State(state): State<AppState>,
    Json(req): Json<GrantRequest>,
) -> Result<Json<GrantResponse>, (StatusCode, String)> {
    let operations: Result<Vec<Operation>, String> = req
        .operations
        .iter()
        .map(|op| match op.as_str() {
            "read" => Ok(Operation::Read),
            "write" => Ok(Operation::Write),
            "subjects" => Ok(Operation::Subjects),
            "search" => Ok(Operation::Search),
            "admin" => Ok(Operation::Admin),
            other => Err(format!("unknown operation: {other}")),
        })
        .collect();

    let operations = operations.map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let expires_at = req
        .expires_in_secs
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));

    let token = state
        .cap_engine
        .mint(&req.subject, &operations, expires_at, None, None)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GrantResponse {
        token: CapEngine::token_to_base64(&token),
    }))
}

/// Basic store statistics.
async fn stats(State(state): State<AppState>) -> impl IntoResponse {
    let subjects = state.store.subjects(None, false).await.unwrap_or_default();

    Json(serde_json::json!({
        "subject_count": subjects.len(),
    }))
}
