//! Streamable-HTTP transport for the ctxd MCP service.
//!
//! Implements the MCP 2025-03-26 / 2025-06-18 single-endpoint transport
//! by wrapping rmcp's [`StreamableHttpService`]. A single `/mcp` route
//! handles JSON-RPC POSTs and supports SSE upgrades for streaming.
//!
//! See the module-level docs in [`crate::transport`] for the auth
//! precedence rules — those are enforced by
//! [`super::auth_middleware`], applied as an axum layer in front of
//! the rmcp service.
//!
//! Tracing: every request logs `remote_addr`, `method`, and (for
//! `tools/call`) the tool name. Auth headers and token bytes are
//! never logged.

use crate::auth::AuthPolicy;
use crate::transport::auth_middleware::{auth_layer, AuthMiddlewareConfig};
use crate::CtxdMcpServer;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Run a streamable-HTTP MCP server on `addr`.
///
/// The server exposes a single `/mcp` endpoint that accepts:
///
/// * `POST /mcp` — JSON-RPC requests. Stateless mode: one request per
///   call. Stateful mode: opens a session via the `initialize`
///   message and threads subsequent calls through the same session
///   identified by `Mcp-Session-Id`. We default to **stateless** so
///   each tool invocation is independent — sessions add memory cost
///   without buying us anything yet.
/// * `GET /mcp` — open a server-to-client SSE stream (only meaningful
///   in stateful mode; we leave it on for compatibility but expect
///   most calls to be stateless POSTs).
/// * `DELETE /mcp` — terminate a session.
///
/// `cancel` is the parent shutdown token: when cancelled, the server
/// drains in-flight requests and returns. Cancelling the **child**
/// token returned in the future would not stop axum, so the caller is
/// responsible for triggering shutdown via `cancel.cancel()`.
pub async fn run_streamable_http(
    server: CtxdMcpServer,
    addr: SocketAddr,
    policy: AuthPolicy,
    cancel: CancellationToken,
) -> Result<(), super::TransportError> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| super::TransportError::Startup(format!("bind {addr}: {e}")))?;

    let bound = listener
        .local_addr()
        .map_err(|e| super::TransportError::Startup(format!("local_addr: {e}")))?;
    tracing::info!(addr = %bound, "streamable-HTTP MCP listening");

    // The factory closure is called once per session by rmcp. Our
    // CtxdMcpServer is already cheap to clone (Arc-backed inner state),
    // so we hand each session its own logical handle on the same
    // store + cap engine.
    let factory_server = server.clone();
    let service: StreamableHttpService<CtxdMcpServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(factory_server.clone()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                // Stateless keeps things simple — we don't yet care
                // about per-session priming or resumable streams.
                .with_stateful_mode(false)
                // JSON response over SSE keeps integration with simple
                // clients (curl, reqwest::Client::post) snappy. SSE
                // is still negotiable per-request via Accept.
                .with_json_response(true)
                // Fold our shutdown signal into the service — when the
                // outer cancel fires, the service tears down.
                .with_cancellation_token(cancel.clone()),
        );

    let auth_cfg = AuthMiddlewareConfig::new(policy);

    let app = Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(
            auth_cfg,
            auth_layer,
        ));

    let make_service = app.into_make_service_with_connect_info::<SocketAddr>();
    let serve_fut = axum::serve(listener, make_service)
        .with_graceful_shutdown(async move { cancel.cancelled_owned().await });

    serve_fut
        .await
        .map_err(|e| super::TransportError::Runtime(format!("axum serve: {e}")))?;

    Ok(())
}
