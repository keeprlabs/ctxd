//! Loopback bypass middleware for the dashboard.
//!
//! The flow:
//!   loopback peer (127.0.0.0/8 or ::1) → allow
//!   else → require an admin cap-token (existing ctxd-cap behavior)
//!
//! Apply via [`apply_localhost_or_cap_token`] to the merged dashboard
//! + ctxd-http router in `ctxd-cli/src/serve.rs` (step 6). For routes
//! that already require admin (e.g. /v1/grant), this middleware is
//! a no-op for token-bearing callers — they already pass.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header::AUTHORIZATION, Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use ctxd_cap::{CapEngine, Operation};
use std::net::SocketAddr;
use std::sync::Arc;

/// State threaded through the middleware. Wrapped in an `Arc` because
/// axum's `from_fn_with_state` requires `Clone + Send + Sync`.
#[derive(Clone)]
pub struct LocalhostOrCapToken {
    cap_engine: Arc<CapEngine>,
}

impl LocalhostOrCapToken {
    /// Build a new layer state. The cap engine is the same one
    /// `ctxd-http`'s `AppState` carries — typically `Arc::clone`'d
    /// from the daemon's shared engine.
    pub fn new(cap_engine: Arc<CapEngine>) -> Self {
        Self { cap_engine }
    }
}

/// The middleware function. Allows when the TCP peer is loopback;
/// otherwise requires an admin cap token in the `Authorization` header.
pub async fn localhost_or_cap_token(
    axum::extract::State(state): axum::extract::State<LocalhostOrCapToken>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    // ConnectInfo<SocketAddr> rides as an extension on the request,
    // populated only when the bind site uses
    // `into_make_service_with_connect_info::<SocketAddr>()`. We pull
    // it manually rather than via the `ConnectInfo` extractor because
    // axum's from_fn middleware tuple arity is awkward when one of
    // the extractors is optional. Failure to find connect-info here
    // means the daemon is misconfigured; we fail closed.
    let peer = match req.extensions().get::<ConnectInfo<SocketAddr>>() {
        Some(ConnectInfo(addr)) => *addr,
        None => {
            tracing::error!(
                "localhost_or_cap_token: ConnectInfo missing — bind site \
                 must use into_make_service_with_connect_info::<SocketAddr>()"
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "connect-info missing").into_response();
        }
    };

    if peer.ip().is_loopback() {
        return next.run(req).await;
    }

    // Non-loopback caller: require an admin token.
    let token_b64 = match req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => {
            let trimmed = s.trim();
            let raw = trimmed.strip_prefix("Bearer ").or_else(|| {
                let (scheme, rest) = trimmed.split_once(char::is_whitespace)?;
                if scheme.eq_ignore_ascii_case("bearer") {
                    Some(rest)
                } else {
                    None
                }
            });
            match raw.map(str::trim).filter(|t| !t.is_empty()) {
                Some(t) => t.to_string(),
                None => {
                    return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
                }
            }
        }
        None => {
            return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
        }
    };

    let token = match CapEngine::token_from_base64(&token_b64) {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::FORBIDDEN, format!("invalid token: {e}")).into_response();
        }
    };
    if let Err(e) = state.cap_engine.verify(&token, "/", Operation::Admin, None) {
        return (StatusCode::FORBIDDEN, e.to_string()).into_response();
    }
    next.run(req).await
}

/// Apply the loopback-or-cap-token layer to a router.
pub fn apply_localhost_or_cap_token<S>(
    router: axum::Router<S>,
    cap_engine: Arc<CapEngine>,
) -> axum::Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(axum::middleware::from_fn_with_state(
        LocalhostOrCapToken::new(cap_engine),
        localhost_or_cap_token,
    ))
}
