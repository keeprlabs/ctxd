//! SQLite-backed event store with materialized views for ctxd.
//!
//! The event log is the source of truth. All materialized views (KV, FTS, vector)
//! are derived from the log and can be rebuilt from it.

pub mod migrations;
pub mod store;
pub mod views;

pub use store::EventStore;
