//! MCP server implementation for ctxd.
//!
//! Exposes ctxd operations as MCP tools over stdio transport:
//! - `ctx.write` — append an event
//! - `ctx.read` — read events for a subject
//! - `ctx.subjects` — list subjects
//!
//! Each tool call takes an optional capability token in its arguments.
//! The MCP server verifies the capability before serving the request.

pub mod server;

pub use server::CtxdMcpServer;
