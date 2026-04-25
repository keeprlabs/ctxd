//! HTTP router and handlers for the ctxd admin API.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use ctxd_cap::state::{ApprovalDecision, CaveatState};
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
    /// Stateful-caveat backing store. Holds budgets and approvals.
    /// `Arc<dyn …>` so the daemon can swap impls (in-memory in tests,
    /// SQLite in `serve`) without changing handler code.
    pub caveat_state: Arc<dyn CaveatState>,
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
    };
    Router::new()
        .route("/health", get(health))
        .route("/v1/grant", post(grant))
        .route("/v1/stats", get(stats))
        .route("/v1/approvals", get(list_approvals))
        .route("/v1/approvals/{id}/decide", post(decide_approval))
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

/// Body for `POST /v1/approvals/:id/decide`.
#[derive(Debug, Deserialize)]
struct DecideRequest {
    /// `"allow"` | `"deny"`.
    decision: String,
    /// Optional admin capability token (base64). Required when the
    /// caller did not present one in a header. When the daemon is
    /// running locally without admin guards (open-by-default, see
    /// ADR 004), this can be omitted.
    token: Option<String>,
}

/// Response from a successful decide.
#[derive(Debug, Serialize)]
struct DecideResponse {
    /// The approval id that was decided.
    approval_id: String,
    /// The decision recorded (`"allow"` or `"deny"`).
    decision: String,
}

/// `POST /v1/approvals/:id/decide` — record a human approval decision.
///
/// Requires an admin token if one is presented. When no token is
/// presented, follows the v0.1 open-by-default semantics (ADR 004) so
/// local CLI users can decide approvals without ceremony.
async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    Json(req): Json<DecideRequest>,
) -> Result<Json<DecideResponse>, (StatusCode, String)> {
    if let Some(token_b64) = req.token.as_ref() {
        let token = CapEngine::token_from_base64(token_b64)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid token: {e}")))?;
        // Admin caps cover any subject; we use "/" as a stable target.
        state
            .cap_engine
            .verify(&token, "/", Operation::Admin, None)
            .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
    }

    let decision = match req.decision.to_ascii_lowercase().as_str() {
        "allow" => ApprovalDecision::Allow,
        "deny" => ApprovalDecision::Deny,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("decision must be 'allow' or 'deny', got '{other}'"),
            ))
        }
    };

    state
        .caveat_state
        .approval_decide(&approval_id, decision)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;

    Ok(Json(DecideResponse {
        approval_id,
        decision: req.decision.to_ascii_lowercase(),
    }))
}

/// `GET /v1/approvals` — list pending approvals.
///
/// Today this is a thin shim that returns an empty list when the
/// caveat-state backend doesn't expose enumeration. The SQLite backend
/// implements [`ApprovalEnumerable`] (see its inherent `pending_approvals`
/// method); the in-memory backend doesn't, and we intentionally don't
/// expose `Vec` of internals via the trait.
async fn list_approvals(State(state): State<AppState>) -> impl IntoResponse {
    // Best-effort enumeration: the trait doesn't carry a list method,
    // so we fall back to `state.store.pending_approvals()` which the
    // SQLite store exposes as a free-standing method on `EventStore`.
    let rows = state
        .store
        .pending_approvals_list()
        .await
        .unwrap_or_default();
    Json(serde_json::json!({ "pending": rows }))
}
