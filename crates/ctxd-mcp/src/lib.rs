//! MCP server implementation for ctxd.
//!
//! Exposes ctxd operations as MCP tools over stdio transport:
//! - `ctx_write` — append an event
//! - `ctx_read` — read events for a subject
//! - `ctx_subjects` — list subjects
//! - `ctx_search` — full-text search
//! - `ctx_subscribe` — poll events since a timestamp
//! - `ctx_entities` — query graph entities (v0.3)
//! - `ctx_related` — traverse graph relationships (v0.3)
//! - `ctx_timeline` — temporal read at a point in time (v0.3)
//!
//! Each tool call takes an optional capability token in its arguments.
//! The MCP server verifies the capability before serving the request.

pub mod server;

pub use server::CtxdMcpServer;
