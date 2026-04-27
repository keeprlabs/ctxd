//! High-level [`CtxdClient`] convenience facade.
//!
//! Wraps the lower-level [`crate::http::HttpAdminClient`] and the
//! lazy-instantiated wire connection so a typical "connect → write →
//! query" flow looks the same as in any modern Rust SDK:
//!
//! ```no_run
//! # async fn run() -> Result<(), ctxd_client::CtxdError> {
//! use ctxd_client::{CtxdClient, QueryView};
//!
//! let client = CtxdClient::connect("http://127.0.0.1:7777").await?
//!     .with_wire("127.0.0.1:7778").await?;
//! let id = client.write("/work/notes/standup", "ctx.note",
//!                       serde_json::json!({"content": "ship Friday"})).await?;
//! let events = client.query("/work/notes", QueryView::Log).await?;
//! assert!(events.iter().any(|e| e.id == id));
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use crate::errors::CtxdError;
use crate::events::{Event, EventId};
use crate::http::{HealthInfo, HttpAdminClient, Operation, PeerInfo, StatsInfo};
use crate::wire::{EventStream, QueryView, WireConn};

/// High-level ctxd client.
///
/// Holds an HTTP admin client (always present) and an optional
/// wire-protocol connection (lazy — opened by [`Self::with_wire`]).
/// Cheap to clone: shared state lives behind `Arc`.
#[derive(Clone)]
pub struct CtxdClient {
    http: HttpAdminClient,
    wire: Option<Arc<Mutex<WireConn>>>,
    wire_addr: Option<String>,
}

impl CtxdClient {
    /// Connect to a ctxd daemon's HTTP admin URL.
    ///
    /// The URL must include scheme + host + port (e.g.
    /// `http://127.0.0.1:7777`). This call only constructs the
    /// underlying `reqwest::Client` — it does **not** issue a network
    /// request. Use [`Self::health`] to verify the daemon is reachable.
    pub async fn connect(http_url: &str) -> Result<Self, CtxdError> {
        let http = HttpAdminClient::new(http_url)?;
        Ok(Self {
            http,
            wire: None,
            wire_addr: None,
        })
    }

    /// Attach a capability token to all admin calls.
    ///
    /// The token is sent as `Authorization: Bearer <token>` on every
    /// HTTP request. Replaces any previously attached token.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.http = self.http.with_token(token);
        self
    }

    /// Connect to the wire protocol (TCP+MsgPack) at `wire_addr`.
    ///
    /// Required for [`Self::write`], [`Self::subscribe`], and
    /// [`Self::query`]. The address is anything
    /// [`tokio::net::TcpStream::connect`] accepts — typically
    /// `host:port`.
    pub async fn with_wire(mut self, wire_addr: &str) -> Result<Self, CtxdError> {
        let conn = WireConn::connect(wire_addr).await?;
        self.wire_addr = Some(conn.addr().to_string());
        self.wire = Some(Arc::new(Mutex::new(conn)));
        Ok(self)
    }

    /// `GET /health` — return the daemon's reported version.
    pub async fn health(&self) -> Result<HealthInfo, CtxdError> {
        self.http.health().await
    }

    /// `GET /v1/stats` — return basic store statistics.
    pub async fn stats(&self) -> Result<StatsInfo, CtxdError> {
        self.http.stats().await
    }

    /// Append an event under `subject` and return its UUID.
    ///
    /// Routed through the wire protocol; requires
    /// [`Self::with_wire`] to have been called first.
    pub async fn write(
        &self,
        subject: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<EventId, CtxdError> {
        let wire = self.wire.as_ref().ok_or(CtxdError::WireNotConfigured)?;
        let mut guard = wire.lock().await;
        guard.write(subject, event_type, data).await
    }

    /// Subscribe to events matching `subject_pattern`. Returns an
    /// [`EventStream`] yielding parsed events as they arrive.
    ///
    /// Internally opens a fresh TCP connection (a subscription puts a
    /// connection into streaming-receive mode and can't be reused for
    /// further requests). Requires [`Self::with_wire`].
    pub async fn subscribe(&self, subject_pattern: &str) -> Result<EventStream, CtxdError> {
        let wire = self.wire.as_ref().ok_or(CtxdError::WireNotConfigured)?;
        let guard = wire.lock().await;
        guard.subscribe(subject_pattern).await
    }

    /// Query a materialized view.
    ///
    /// Returns the event list for `Log` and `Fts` views. The `Kv`
    /// view is not exposed here (its shape is per-subject opaque
    /// JSON, not a list). Drop down to [`crate::wire::WireRequest`]
    /// for raw KV access.
    pub async fn query(
        &self,
        subject_pattern: &str,
        view: QueryView,
    ) -> Result<Vec<Event>, CtxdError> {
        let wire = self.wire.as_ref().ok_or(CtxdError::WireNotConfigured)?;
        let mut guard = wire.lock().await;
        guard.query(subject_pattern, view).await
    }

    /// Mint a capability token (admin endpoint).
    ///
    /// Uses `POST /v1/grant` — the HTTP path is open-by-default in
    /// v0.3 (see ADR 004); production deployments behind a network
    /// boundary should front this with their own auth.
    pub async fn grant(
        &self,
        subject: &str,
        operations: &[Operation],
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<String, CtxdError> {
        self.http.grant(subject, operations, expires_at).await
    }

    /// Revoke a capability token by id.
    ///
    /// In v0.3 this is wired through the wire-protocol `Revoke` verb
    /// — the HTTP admin API does not yet expose a REST revoke
    /// endpoint. Requires [`Self::with_wire`].
    pub async fn revoke(&self, token_id: &str) -> Result<(), CtxdError> {
        let wire = self.wire.as_ref().ok_or(CtxdError::WireNotConfigured)?;
        let mut guard = wire.lock().await;
        guard.revoke(token_id).await
    }

    /// `GET /v1/peers` — list federation peers (admin).
    pub async fn peers(&self) -> Result<Vec<PeerInfo>, CtxdError> {
        self.http.peers().await
    }

    /// `DELETE /v1/peers/{peer_id}` — remove a federation peer (admin).
    ///
    /// Returns `Err(CtxdError::NotFound)` if no peer with that id
    /// exists.
    pub async fn peer_remove(&self, peer_id: &str) -> Result<(), CtxdError> {
        self.http.peer_remove(peer_id).await
    }

    /// Verify an event's Ed25519 signature against a hex-encoded
    /// public key. Pure function — does not touch the network.
    pub fn verify_signature(event: &Event, pubkey_hex: &str) -> Result<bool, CtxdError> {
        crate::signing::verify_signature(event, pubkey_hex)
    }

    /// Return the wire address this client was configured with, if
    /// any. Useful for diagnostics and for tests that need to reuse
    /// the address.
    pub fn wire_addr(&self) -> Option<&str> {
        self.wire_addr.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_without_wire_returns_clear_error() {
        // We can build the client without ever hitting the network.
        let client = CtxdClient::connect("http://127.0.0.1:1")
            .await
            .expect("connect builds without IO");
        let err = client
            .write("/x", "demo", serde_json::json!({}))
            .await
            .expect_err("must fail with WireNotConfigured");
        assert!(matches!(err, CtxdError::WireNotConfigured));
    }

    #[tokio::test]
    async fn subscribe_without_wire_returns_clear_error() {
        let client = CtxdClient::connect("http://127.0.0.1:1")
            .await
            .expect("connect");
        // EventStream doesn't impl Debug by design (it wraps a live
        // TCP connection), so we match instead of `expect_err`.
        match client.subscribe("/**").await {
            Err(CtxdError::WireNotConfigured) => {}
            Err(other) => panic!("expected WireNotConfigured, got {other:?}"),
            Ok(_) => panic!("expected WireNotConfigured, got Ok"),
        }
    }
}
