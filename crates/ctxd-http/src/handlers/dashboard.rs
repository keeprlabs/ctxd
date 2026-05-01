//! `POST /v1/dashboard/hello-world` — empty-state tutorial write.
//!
//! Single-purpose write that bypasses the usual capability-token
//! check **only when the request is from loopback**. Wired into the
//! global router but enforced by the `localhost_or_cap_token`
//! middleware in `ctxd-dashboard` (Step 5): a non-loopback request
//! that reaches this handler must already have presented a valid
//! admin token. Since this endpoint isn't useful with a token
//! (production use would never send a hardcoded "hello world" event),
//! the handler also explicitly rejects when the request looks
//! cap-token-authenticated.

use crate::handlers::bearer_from_headers;
use crate::router::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;

/// Hardcoded subject for the tutorial event. Stable so a user can
/// later `ctxd query 'FROM e IN events WHERE subject="/dashboard/tutorial/hello"'`
/// to see what the dashboard wrote.
const HELLO_SUBJECT: &str = "/dashboard/tutorial/hello";

/// Hardcoded body for the tutorial event.
const HELLO_BODY: &str =
    "Hello from your ctxd dashboard. This is your first event.";

/// `POST /v1/dashboard/hello-world` — write one fixed event.
///
/// Defense in depth: even though the network-layer middleware should
/// only let loopback through, we belt-and-suspenders here by rejecting
/// any request that carries a bearer token. A real cap-token caller
/// shouldn't be using this endpoint at all — they'd write events
/// directly via the wire protocol or MCP.
#[tracing::instrument(skip(state))]
pub(crate) async fn hello_world(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Event>, (StatusCode, String)> {
    if bearer_from_headers(&headers).is_some() {
        return Err((
            StatusCode::FORBIDDEN,
            "hello-world is a loopback-only tutorial endpoint; remote callers \
             should use the wire protocol or MCP to write events"
                .to_string(),
        ));
    }

    let event = Event::new(
        "ctxd://dashboard".to_string(),
        Subject::new(HELLO_SUBJECT).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        "ctx.note".to_string(),
        serde_json::json!({"content": HELLO_BODY}),
    );

    let stored = state
        .store
        .append(event)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(stored))
}
