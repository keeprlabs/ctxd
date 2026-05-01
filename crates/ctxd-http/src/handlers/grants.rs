//! `POST /v1/grant` — mint a capability token.

use crate::router::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use ctxd_cap::{CapEngine, Operation};
use serde::{Deserialize, Serialize};

/// Request body for the grant endpoint.
#[derive(Debug, Deserialize)]
pub(crate) struct GrantRequest {
    /// Subject glob pattern the token grants access to.
    subject: String,
    /// Operations to grant.
    operations: Vec<String>,
    /// Optional expiry in seconds from now.
    expires_in_secs: Option<i64>,
}

/// Response from the grant endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct GrantResponse {
    /// Base64-encoded capability token.
    token: String,
}

/// Mint a new capability token.
pub(crate) async fn grant(
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
