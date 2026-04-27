//! # ctxd-client
//!
//! Official Rust SDK for the [ctxd](https://github.com/keeprlabs/ctxd)
//! context substrate daemon.
//!
//! `ctxd` is the append-only event log + capability layer that AI
//! agents talk to over MCP, the wire protocol, or HTTP. This crate is
//! the Rust SDK every consumer reaches for first: it gives you a
//! single [`CtxdClient`] that knows how to mix the HTTP admin surface
//! (health, grant, peers, stats) with the wire protocol (write,
//! subscribe, query) — without making you stitch them together.
//!
//! ## Quickstart
//!
//! ```no_run
//! use ctxd_client::{CtxdClient, Operation, QueryView};
//!
//! # async fn run() -> Result<(), ctxd_client::CtxdError> {
//! let client = CtxdClient::connect("http://127.0.0.1:7777").await?
//!     .with_wire("127.0.0.1:7778").await?;
//!
//! // Write an event.
//! let id = client
//!     .write("/work/notes/standup", "ctx.note",
//!            serde_json::json!({"content": "ship Friday"}))
//!     .await?;
//!
//! // Read it back.
//! let events = client.query("/work/notes", QueryView::Log).await?;
//! assert!(events.iter().any(|e| e.id == id));
//!
//! // Mint a scoped, read-only token for an agent.
//! let token = client
//!     .grant("/work/notes/**", &[Operation::Read, Operation::Subjects], None)
//!     .await?;
//! # let _ = token;
//! # Ok(())
//! # }
//! ```
//!
//! ## Crate layout
//!
//! - [`CtxdClient`] — the high-level facade you'll use 95% of the time.
//! - [`http::HttpAdminClient`] — the lower-level HTTP admin client if
//!   you want to bypass the wire protocol entirely.
//! - [`wire::WireConn`] (private) plus the typed wrappers and the
//!   [`wire::EventStream`] subscription type.
//! - [`signing::verify_signature`] — pure-function Ed25519 verifier
//!   matching the canonical bytes the daemon signs over.
//!
//! ## TLS
//!
//! HTTPS support is `rustls`-only by design. We deliberately do not
//! pull in `native-tls` / OpenSSL — the SDK should not require system
//! crypto libraries on its host.
//!
//! ## MSRV
//!
//! Rust 1.78. CI pins this in `clippy.toml` once we cut a release.
//!
//! ## License
//!
//! Apache-2.0.

#![deny(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod client;
pub mod errors;
pub mod events;
pub mod http;
pub mod signing;
pub mod wire;

pub use client::CtxdClient;
pub use errors::CtxdError;
pub use events::{Event, EventId, Subject};
pub use http::{HealthInfo, Operation, PeerInfo, StatsInfo};
pub use signing::verify_signature;
pub use wire::{EventStream, QueryView};
