//! Transports for the ctxd MCP service.
//!
//! Three transports expose the same [`crate::CtxdMcpServer`] tool surface:
//!
//! * **stdio** — newline-delimited JSON-RPC over stdin/stdout. Suitable
//!   for Claude Desktop, mcp-inspector, and other local subprocess
//!   clients. See [`run_stdio`].
//! * **SSE** — legacy MCP HTTP/SSE pair: clients GET `/sse` to open a
//!   server-to-client event stream, then POST JSON-RPC requests to
//!   `/messages?session_id=…`. See [`run_sse`] (gated by the
//!   `http-transports` Cargo feature).
//! * **streamable HTTP** — the MCP 2025-03-26 / 2025-06-18 unified
//!   transport: a single `/mcp` endpoint that handles JSON-RPC POSTs
//!   and supports SSE upgrades for streaming. See
//!   [`run_streamable_http`] (also gated by `http-transports`).
//!
//! All three accept the same capability tokens via either an
//! `Authorization: Bearer <base64-biscuit>` header (HTTP transports
//! only) or a per-tool-call `token` argument. The header wins when both
//! are present — see [`crate::auth`] for the precedence rule.
//!
//! Tracing: every request entering an HTTP transport logs at INFO with
//! `remote_addr`, `method`, and `tool_name`. Authorization headers and
//! token bytes are **never** logged.

use crate::CtxdMcpServer;
use rmcp::ServiceExt;

/// Run the MCP server over stdio, blocking the calling task until the
/// peer disconnects or an I/O error occurs.
///
/// This is the v0.1 transport and the path used by Claude Desktop. It
/// has no auth header — token enforcement is done per-tool via the
/// `token` argument.
///
/// Returns once the underlying [`rmcp::transport::io::stdio`] transport
/// has finished serving, propagating any startup error.
pub async fn run_stdio(server: CtxdMcpServer) -> Result<(), TransportError> {
    let transport = rmcp::transport::io::stdio();
    let running = server
        .serve(transport)
        .await
        .map_err(|e| TransportError::Startup(format!("stdio: {e}")))?;
    let _ = running.waiting().await;
    Ok(())
}

/// Errors common to all transports.
///
/// Each transport's `run_*` function returns this type so the caller in
/// `ctxd-cli` can log + decide whether to abort the daemon. We
/// deliberately do not collapse these into `anyhow::Error` to keep
/// downstream pattern-matching practical.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The transport failed to bind or initialise.
    #[error("transport failed to start: {0}")]
    Startup(String),

    /// The transport bound successfully but errored during request
    /// handling. This is logged but does not crash the daemon — sibling
    /// transports keep running.
    #[error("transport runtime error: {0}")]
    Runtime(String),
}

#[cfg(feature = "http-transports")]
pub mod sse;
#[cfg(feature = "http-transports")]
pub mod streamable_http;

#[cfg(feature = "http-transports")]
pub use sse::run_sse;
#[cfg(feature = "http-transports")]
pub use streamable_http::run_streamable_http;

#[cfg(feature = "http-transports")]
mod auth_middleware;

/// Default maximum JSON-RPC body size (bytes) that HTTP transports will
/// accept per request. Anything larger gets a 413 Payload Too Large.
///
/// This is a defence against simple DoS — a clueless or hostile client
/// shouldn't be able to exhaust daemon memory by sending a multi-gigabyte
/// "tools/call". 1 MiB is generous for our tool arguments (the largest
/// reasonable payload is a `ctx_write` `data` field that is itself
/// JSON-encoded; a megabyte covers everything we care about).
#[cfg(feature = "http-transports")]
pub const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
