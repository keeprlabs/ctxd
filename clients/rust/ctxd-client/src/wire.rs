//! Wire-protocol client.
//!
//! Thin ergonomic wrapper around [`ctxd_wire::ProtocolClient`]. The
//! lower-level crate is the source of truth for the framing + types;
//! this module only adds:
//!
//! - Strongly-typed [`crate::events::Event`] decoding for `Pub`,
//!   `Query`, and `Subscribe` responses (the wire crate carries
//!   payloads as opaque `serde_json::Value` to keep its dependency
//!   surface minimal).
//! - A [`futures::Stream`]-shaped [`EventStream`] over the
//!   subscription connection.
//! - Convenience aliases that re-export the wire types so SDK users
//!   never have to add a direct `ctxd-wire` dependency just to name a
//!   `Request` or `Response`.

use ctxd_wire::{ProtocolClient, Response, SubscriptionStream};

use crate::errors::CtxdError;
use crate::events::{Event, EventId};

// Re-exports for SDK consumers who want to drop down to the raw wire
// types (writing custom request types, building federation tooling,
// etc.).
pub use ctxd_wire::{Request as WireRequest, Response as WireResponse, WireError};

/// Materialized view name supported by the daemon's `QUERY` verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryView {
    /// Append-only log view — returns the events themselves.
    Log,
    /// Key-value view — last-write-wins per subject.
    Kv,
    /// Full-text-search view — events matching a query string.
    Fts,
}

impl QueryView {
    /// Wire-format name accepted by the daemon's `QUERY` verb.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Kv => "kv",
            Self::Fts => "fts",
        }
    }
}

/// A live subscription stream of events.
///
/// The primary API is [`Self::next_event`], an async method that
/// yields the next event or `Ok(None)` when the daemon ends the
/// stream. Drop the [`EventStream`] to close the underlying TCP
/// connection — the daemon stops sending further events for that
/// pattern.
///
/// We deliberately do **not** implement [`futures::Stream`] in v0.3.
/// The wire crate's [`SubscriptionStream::next_event`] borrows
/// `&mut self`, and adapting that into a `Stream::poll_next` cleanly
/// requires either a state machine or `unsafe` self-referential code.
/// The bare async method is the simpler, safer surface; we'll add a
/// `Stream` impl in v0.4 once `async-fn-in-trait`-shaped streams
/// stabilize the pattern.
pub struct EventStream {
    inner: SubscriptionStream,
}

impl EventStream {
    /// Build an [`EventStream`] from a raw [`SubscriptionStream`].
    pub(crate) fn new(inner: SubscriptionStream) -> Self {
        Self { inner }
    }

    /// Read the next event. Returns `Ok(None)` when the daemon
    /// signals end-of-stream or the connection is closed cleanly.
    pub async fn next_event(&mut self) -> Result<Option<Event>, CtxdError> {
        match self.inner.next_event().await? {
            None => Ok(None),
            Some(Response::Event { event }) => {
                let parsed: Event = serde_json::from_value(event)?;
                Ok(Some(parsed))
            }
            Some(Response::Error { message }) => Err(CtxdError::UnexpectedWireResponse(message)),
            Some(other) => Err(CtxdError::UnexpectedWireResponse(format!(
                "expected Event, got {other:?}"
            ))),
        }
    }
}

/// Convenience handle for the wire-protocol verbs the SDK exposes
/// directly. Owns a [`ProtocolClient`] and routes the typed methods
/// through it.
pub(crate) struct WireConn {
    client: ProtocolClient,
    addr: String,
}

impl WireConn {
    /// Open a fresh connection to the daemon's wire protocol.
    pub async fn connect(addr: &str) -> Result<Self, CtxdError> {
        let client = ProtocolClient::connect(addr).await?;
        Ok(Self {
            client,
            addr: addr.to_string(),
        })
    }

    /// Connection address (used by [`crate::CtxdClient`] to open
    /// fresh connections for one-shot subscriptions).
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Append an event under `subject` and return its UUID.
    pub async fn write(
        &mut self,
        subject: &str,
        event_type: &str,
        data: serde_json::Value,
    ) -> Result<EventId, CtxdError> {
        let resp = self.client.publish(subject, event_type, data).await?;
        match resp {
            Response::Ok { data } => {
                // The PUB handler returns the full event JSON. Pull
                // the id field — it's a UUIDv7 string per CloudEvents.
                let id_str = data
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        CtxdError::UnexpectedWireResponse(format!(
                            "Pub response missing string `id` field: {data}"
                        ))
                    })?;
                let id = uuid::Uuid::parse_str(id_str).map_err(|e| {
                    CtxdError::UnexpectedWireResponse(format!("invalid uuid in Pub response: {e}"))
                })?;
                Ok(id)
            }
            Response::Error { message } => Err(CtxdError::UnexpectedWireResponse(message)),
            other => Err(CtxdError::UnexpectedWireResponse(format!(
                "expected Ok, got {other:?}"
            ))),
        }
    }

    /// Query a materialized view. Returns the parsed events for the
    /// `log` and `fts` views; the `kv` view is not exposed here (its
    /// shape is per-subject opaque JSON, not a list of events).
    pub async fn query(
        &mut self,
        subject_pattern: &str,
        view: QueryView,
    ) -> Result<Vec<Event>, CtxdError> {
        let resp = self
            .client
            .query(subject_pattern, view.as_wire_str())
            .await?;
        match resp {
            Response::Ok { data } => {
                if matches!(view, QueryView::Kv) {
                    // The KV view returns a single value, not an array.
                    // The SDK's `query` is the "list of events" entry
                    // point, so we surface the mismatch as a clear
                    // error rather than guessing a shape.
                    return Err(CtxdError::UnexpectedWireResponse(
                        "kv view returns a value, not a list of events; use Wire APIs directly"
                            .to_string(),
                    ));
                }
                let events: Vec<Event> = serde_json::from_value(data)?;
                Ok(events)
            }
            Response::Error { message } => Err(CtxdError::UnexpectedWireResponse(message)),
            other => Err(CtxdError::UnexpectedWireResponse(format!(
                "expected Ok, got {other:?}"
            ))),
        }
    }

    /// Revoke a capability token via the wire `Revoke` verb.
    pub async fn revoke(&mut self, token_id: &str) -> Result<(), CtxdError> {
        let resp = self
            .client
            .request(&ctxd_wire::Request::Revoke {
                cap_id: token_id.to_string(),
            })
            .await?;
        match resp {
            Response::Ok { .. } => Ok(()),
            Response::Error { message } => Err(CtxdError::UnexpectedWireResponse(message)),
            other => Err(CtxdError::UnexpectedWireResponse(format!(
                "expected Ok, got {other:?}"
            ))),
        }
    }

    /// Open a fresh connection to the same wire address and turn it
    /// into a subscription stream.
    ///
    /// We open a fresh TCP connection per subscription because a
    /// `Sub` puts the underlying socket into streaming-receive mode
    /// — the existing `WireConn` socket must stay free for further
    /// request/response calls.
    pub async fn subscribe(&self, subject_pattern: &str) -> Result<EventStream, CtxdError> {
        let client = ProtocolClient::connect(&self.addr).await?;
        let sub = client.subscribe(subject_pattern).await?;
        Ok(EventStream::new(sub))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_view_strings_match_wire() {
        assert_eq!(QueryView::Log.as_wire_str(), "log");
        assert_eq!(QueryView::Kv.as_wire_str(), "kv");
        assert_eq!(QueryView::Fts.as_wire_str(), "fts");
    }
}
