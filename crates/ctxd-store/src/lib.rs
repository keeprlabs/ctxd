//! Back-compat re-export shim for the default SQLite backend.
//!
//! Pre-v0.3, `ctxd-store` was the single SQLite-backed implementation.
//! In v0.3 the storage layer was split:
//!
//! - [`ctxd-store-core`](../ctxd_store_core/index.html) — the [`Store`](ctxd_store_core::Store)
//!   trait and shared conformance suite.
//! - [`ctxd-store-sqlite`](../ctxd_store_sqlite/index.html) — the default SQLite backend.
//!
//! This crate now exists purely so `use ctxd_store::EventStore` keeps
//! working while call sites migrate. New code should depend on
//! `ctxd-store-core` (for the trait) plus a concrete backend.

pub use ctxd_store_sqlite::*;

/// Re-export of the trait crate so `ctxd_store::core::Store` resolves.
pub mod core {
    pub use ctxd_store_core::*;
}
