//! ctxd wire protocol: MessagePack over TCP.
//!
//! This crate is the **lean leaf** of the ctxd workspace: it carries the
//! over-the-wire types ([`Request`], [`Response`], [`BroadcastEvent`]),
//! the length-prefix frame codec ([`read_frame`], [`write_frame`]), and a
//! TCP [`ProtocolClient`] suitable for any consumer — the daemon's own
//! integration tests, the upcoming `ctxd-client` Rust SDK, or third-party
//! tooling that wants to talk to a `ctxd serve` daemon.
//!
//! The crate intentionally has no dependency on the daemon-side stack
//! (`ctxd-store`, `ctxd-cap`, `ctxd-mcp`, `ctxd-http`, axum, rmcp,
//! sqlx). Adding any of those would defeat the purpose: SDK clients
//! must be able to depend on `ctxd-wire` without inheriting a server.
//!
//! The six wire verbs are:
//!
//! - `PUB <subject> <event_type> <data>` — append event
//! - `SUB <subject_pattern>` — subscribe (returns stream of events)
//! - `QUERY <subject_pattern> <view>` — query materialized view
//! - `GRANT <subject> <ops> <expiry>` — mint capability token
//! - `REVOKE <cap_id>` — stub (v0.2)
//! - `PING` — health check
//!
//! Plus the v0.3 federation verbs (`PeerHello`, `PeerWelcome`,
//! `PeerReplicate`, `PeerAck`, `PeerCursorRequest`, `PeerCursor`,
//! `PeerFetchEvents`).

pub mod client;
pub mod errors;
pub mod frame;
pub mod messages;

pub use client::{ProtocolClient, SubscriptionStream};
pub use errors::{Result, WireError};
pub use frame::{read_frame, write_frame, MAX_FRAME_BYTES};
pub use messages::{BroadcastEvent, Request, Response};
