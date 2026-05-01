//! Per-endpoint HTTP handlers, split out of `router.rs` so each
//! endpoint owns its types, error mapping, and tests in one file.
//!
//! `router.rs` keeps shared `AppState` + `build_router()`; everything
//! else lives here.

pub mod approvals;
pub mod dashboard;
pub mod events;
pub mod grants;
pub mod health;
pub mod peers;
pub mod search;
pub mod stats;
pub mod subjects;

use axum::http::{header::AUTHORIZATION, HeaderMap, StatusCode};
use ctxd_cap::{CapEngine, Operation};

/// Extract a base64 biscuit from the `Authorization: Bearer <token>`
/// header. Returns `None` if the header is missing, multi-valued, has
/// non-ASCII bytes, or doesn't follow the `Bearer <token>` shape.
///
/// The header value is intentionally never logged — bearer tokens are
/// secrets.
pub(crate) fn bearer_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?;
    let s = value.to_str().ok()?;
    let trimmed = s.trim();
    let token = trimmed.strip_prefix("Bearer ").or_else(|| {
        // Be lenient about the case of the scheme — RFC 7235 says
        // schemes are case-insensitive.
        let (scheme, rest) = trimmed.split_once(char::is_whitespace)?;
        if scheme.eq_ignore_ascii_case("bearer") {
            Some(rest)
        } else {
            None
        }
    })?;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// Verify that the request carries a bearer token granting
/// [`Operation::Admin`] for any subject (we use `"/"` as the canonical
/// admin target — admin caps cover any subject glob).
///
/// Returns `Err` mapped directly to the appropriate HTTP status:
/// - missing or malformed `Authorization` → `401 Unauthorized`
/// - present but lacking the admin scope (or otherwise invalid) →
///   `403 Forbidden`
pub(crate) fn require_admin(
    cap_engine: &CapEngine,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, String)> {
    let token_b64 = bearer_from_headers(headers)
        .ok_or_else(|| (StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;
    let token = CapEngine::token_from_base64(&token_b64)
        .map_err(|e| (StatusCode::FORBIDDEN, format!("invalid token: {e}")))?;
    cap_engine
        .verify(&token, "/", Operation::Admin, None)
        .map_err(|e| (StatusCode::FORBIDDEN, e.to_string()))?;
    Ok(())
}
