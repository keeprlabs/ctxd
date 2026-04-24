//! SQLite-backed event store with materialized views for ctxd.
//!
//! The event log is the source of truth. All materialized views (KV, FTS, vector)
//! are derived from the log and can be rebuilt from it.
//!
//! This crate provides:
//!
//! - [`EventStore`] — a concrete `Clone`-able handle around a SQLite pool.
//! - An `impl` of [`ctxd_store_core::Store`] for [`EventStore`] so callers
//!   can pass `Arc<dyn Store>` when they want runtime-selectable backends.
//!
//! New code should prefer the trait. Existing call sites may keep using
//! the concrete type — the public API is stable across v0.2 and v0.3.

pub mod migrate;
pub mod migrations;
pub mod store;
pub mod store_trait;
pub mod views;

pub use store::{EventStore, StoreError};
