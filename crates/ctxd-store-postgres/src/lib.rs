//! Postgres-backed event store for ctxd.
//!
//! Implements [`ctxd_store_core::Store`] over PostgreSQL with the same
//! semantics as `ctxd-store-sqlite`. The event log is the source of
//! truth; KV / FTS / vector / graph views are derived materializations
//! kept in lock-step with `events` via per-append transactions.
//!
//! ## Schema
//!
//! Migrations live in [`migrations/`](https://github.com/ctxd/ctxd/tree/main/crates/ctxd-store-postgres/migrations)
//! and are applied at startup by [`PostgresStore::new`]. They are
//! idempotent (safe to re-run) — every `CREATE` statement is gated on
//! `IF NOT EXISTS`.
//!
//! ## Concurrency
//!
//! Append correctness depends on a serializable view of "the last event
//! for this subject". We use a Postgres `pg_advisory_xact_lock` keyed on
//! the subject hash to serialize concurrent appenders on the same
//! subject without forcing the whole table into a single writer
//! (see ADR 016).
//!
//! ## Vector search
//!
//! v0.3 uses an in-process HNSW index built lazily over `BYTEA` raw
//! vectors. Phase 5 ships pgvector integration as v0.4; until then the
//! linear scan path matches `ctxd-store-sqlite`'s semantics exactly so
//! the conformance suite passes byte-identically.

#![warn(missing_docs)]

pub mod caveat_state;
pub mod fts;
pub mod schema;
pub mod store;
pub mod store_trait;
pub mod vector;

pub use caveat_state::PostgresCaveatState;
pub use store::{PostgresStore, StoreError};
