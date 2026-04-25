//! MCP server implementation for ctxd.
//!
//! Exposes ctxd operations as MCP tools over three transports:
//!
//! * **stdio** — newline-delimited JSON-RPC over stdin/stdout
//!   (always available).
//! * **SSE** — legacy `GET /sse` + `POST /messages` shape
//!   (gated by the `http-transports` Cargo feature).
//! * **streamable HTTP** — modern unified `/mcp` endpoint
//!   (also gated by `http-transports`).
//!
//! All three transports serve the same [`CtxdMcpServer`] tool surface:
//!
//! - `ctx_write` — append an event
//! - `ctx_read` — read events for a subject
//! - `ctx_subjects` — list subjects
//! - `ctx_search` — full-text search
//! - `ctx_subscribe` — poll events since a timestamp
//! - `ctx_entities` — query graph entities (v0.3)
//! - `ctx_related` — traverse graph relationships (v0.3)
//! - `ctx_timeline` — temporal read at a point in time (v0.3)
//!
//! Each tool call carries an optional capability token. On the HTTP
//! transports, an `Authorization: Bearer <base64-biscuit>` header
//! takes precedence over a per-call `token` argument; see
//! [`auth`] for the precedence policy.

pub mod auth;
pub mod server;
pub mod transport;

pub use server::CtxdMcpServer;
