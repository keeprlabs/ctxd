//! Unified error type for the [`crate`] SDK.
//!
//! The SDK glues three independent error surfaces — the HTTP admin
//! client (`reqwest`), the wire protocol (`ctxd_wire::WireError`), and
//! Ed25519 signature verification — into a single [`CtxdError`] so
//! callers can `?` through SDK calls without juggling types.
//!
//! All variants are non-exhaustive in spirit: we will add new variants
//! as the API grows. SDK consumers should match on the variants they
//! care about and treat the rest as opaque transport / decode errors.

use thiserror::Error;

/// Errors raised by the ctxd Rust SDK.
#[derive(Debug, Error)]
pub enum CtxdError {
    /// Underlying HTTP transport or non-2xx response from the admin API.
    ///
    /// Carries the raw reqwest error verbatim — the SDK never strips
    /// status codes or response bodies, so callers retain the full
    /// diagnostic surface.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    /// HTTP request returned a non-success status with a plain-text
    /// body. Distinct from [`CtxdError::Http`] (which is reqwest's own
    /// transport / decode error) — this variant surfaces a *server*
    /// error that completed the round-trip cleanly.
    #[error("http {status}: {body}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// Response body (typically a one-line plain-text message
        /// from the daemon).
        body: String,
    },

    /// Wire-protocol IO / codec failure. Wrapped from
    /// [`ctxd_wire::WireError`] verbatim.
    #[error("wire error: {0}")]
    Wire(#[from] ctxd_wire::WireError),

    /// Ed25519 signature verification failure.
    ///
    /// The variant carries a short reason string. The SDK does NOT
    /// surface ed25519-dalek's underlying error verbatim because those
    /// errors deliberately don't disclose which check failed — that's
    /// the library's hardening against side-channel attacks, and we
    /// preserve it.
    #[error("signature verification failed: {0}")]
    Signing(String),

    /// JSON decode / encode failure mapping wire payloads.
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),

    /// Authorization rejected by the server (HTTP 401/403). Distinct
    /// from a generic [`CtxdError::HttpStatus`] so the caller can
    /// distinguish bad credentials from other failures.
    #[error("authorization rejected: {0}")]
    Auth(String),

    /// Server returned 404 for the requested resource.
    #[error("not found: {0}")]
    NotFound(String),

    /// The SDK was used in a way that requires a wire-protocol
    /// connection but only the HTTP client is configured.
    /// Returned from [`crate::CtxdClient::write`], `subscribe`,
    /// `query` and friends if the caller forgot
    /// [`crate::CtxdClient::with_wire`].
    #[error("wire client not configured: call CtxdClient::with_wire(addr).await first")]
    WireNotConfigured,

    /// The server's wire-protocol response did not match what the SDK
    /// expected (e.g. a `Pub` came back as `Error` or `Pong`).
    #[error("unexpected wire response: {0}")]
    UnexpectedWireResponse(String),
}

impl CtxdError {
    /// Construct a [`CtxdError::HttpStatus`] from a status code and body.
    pub(crate) fn http_status(status: reqwest::StatusCode, body: String) -> Self {
        match status.as_u16() {
            401 | 403 => Self::Auth(body),
            404 => Self::NotFound(body),
            other => Self::HttpStatus { status: other, body },
        }
    }
}
