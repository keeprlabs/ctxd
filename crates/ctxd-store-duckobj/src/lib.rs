//! DuckDB-on-object-store backend for ctxd.
//!
//! The event log is stored as append-only Parquet files on an object
//! store (S3, R2, local filesystem) fronted by an in-process
//! write-ahead log and buffered Arrow RecordBatch writer. Small
//! transactional state (KV-view, peers, peer_cursors, revoked_tokens,
//! vector_embeddings) lives in a local SQLite sidecar so federation
//! LWW semantics match the other backends byte-for-byte.
//!
//! See [`docs/decisions/018-duckdb-object-store-backend.md`] in the
//! repo for the architecture decision record.
//!
//! ## Durability model
//!
//! 1. `append()` pushes the event into an in-memory buffer AND to
//!    the local `events.wal` before returning.
//! 2. A background flush task seals the buffer into a Parquet part
//!    when the buffer crosses 1000 events OR 16 MB serialized OR 1
//!    second elapses.
//! 3. The sealed part name is recorded in `_manifest.json`. The
//!    manifest update is the integrity boundary: a part file without
//!    a manifest entry is invisible to readers and garbage-collectable.
//! 4. On startup the WAL is replayed to rehydrate the in-memory
//!    buffer so crashed-between-append-and-flush events survive.
//!
//! ## Why a sidecar SQLite?
//!
//! Parquet is read-mostly; updating "the latest KV value for subject X"
//! is a random write pattern Parquet is terrible at. Keeping the
//! transactional views in a local SQLite sidecar gives us byte-identical
//! federation LWW behaviour at a tiny operational cost (one extra file
//! to back up alongside the object-store bucket).

#![warn(missing_docs)]

pub mod manifest;
pub mod parquet_io;
pub mod sidecar;
pub mod store;
pub mod store_trait;
pub mod wal;

pub use store::{DuckObjConfig, DuckObjStore, StoreError};
