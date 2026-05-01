//! `GET /v1/approvals` and `POST /v1/approvals/:id/decide` — pending
//! human-approval list and decisions.

use crate::handlers;
use crate::router::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use ctxd_cap::state::ApprovalDecision;
use ctxd_cap::{CapEngine, Operation};
use serde::{Deserialize, Serialize};

/// Body for `POST /v1/approvals/:id/decide`.
#[derive(Debug, Deserialize)]
pub(crate) struct DecideRequest {
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
pub(crate) struct DecideResponse {
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
pub(crate) async fn decide_approval(
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
pub(crate) async fn list_approvals(State(state): State<AppState>) -> impl IntoResponse {
    let rows = state
        .store
        .pending_approvals_list()
        .await
        .unwrap_or_default();
    Json(serde_json::json!({ "pending": rows }))
}

// Silence dead-code lint: the helpers module is referenced for symmetry.
#[allow(dead_code)]
fn _link() {
    let _ = handlers::bearer_from_headers;
}
