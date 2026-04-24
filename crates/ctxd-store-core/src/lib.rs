//! Storage trait and conformance test suite for ctxd backends.
//!
//! `ctxd-store-core` defines the [`Store`] trait that every backend
//! (SQLite, Postgres, DuckDB+object-store, memory, ...) must implement.
//! The trait is `async_trait`-based and designed to be object-safe so
//! runtime backend selection via `dyn Store` is possible.
//!
//! See [`testsuite`] for the shared conformance test suite that every
//! backend re-uses from its own tests.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod testsuite;

/// Errors surfaced by a [`Store`] implementation.
///
/// Concrete backends wrap their native error types; callers that need the
/// underlying type should downcast via [`StoreError::Other`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The store rejected an append because the supplied predecessor
    /// hash did not match the canonical predecessor.
    #[error("hash chain violation: expected predecessor hash {expected}, got {actual}")]
    HashChainViolation {
        /// Expected predecessor hash (hex).
        expected: String,
        /// Actual predecessor hash observed (hex).
        actual: String,
    },

    /// A well-formed subject path was expected and we received something else.
    #[error("subject error: {0}")]
    Subject(#[from] ctxd_core::subject::SubjectError),

    /// JSON (de)serialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Backend-native error. Downcast through the inner `Box` to inspect.
    #[error("store backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    /// Any other error that doesn't fit a more specific variant.
    #[error("{0}")]
    Other(String),
}

impl StoreError {
    /// Convenience wrapper for backend-specific errors.
    pub fn backend<E>(e: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend(Box::new(e))
    }
}

/// A registered federation peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Peer {
    /// Local identifier for this peer (free-form, often the remote pubkey hex).
    pub peer_id: String,
    /// URL we dial when replicating with this peer (e.g. `tcp://host:port`).
    pub url: String,
    /// Remote peer's Ed25519 public key, 32 raw bytes.
    pub public_key: Vec<u8>,
    /// Subject globs we're willing to deliver to this peer.
    pub granted_subjects: Vec<String>,
    /// Trust-level metadata — free-form JSON for future policy evolution.
    pub trust_level: serde_json::Value,
    /// Timestamp the peer was first registered.
    pub added_at: DateTime<Utc>,
}

/// A replication cursor recording the last event we exchanged with a peer
/// for a particular subject pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerCursor {
    /// The peer this cursor belongs to.
    pub peer_id: String,
    /// Subject glob pattern the cursor applies to.
    pub subject_pattern: String,
    /// Event id of the last event exchanged, or `None` if we haven't
    /// exchanged anything yet for this subject pattern.
    pub last_event_id: Option<Uuid>,
    /// Timestamp of the last event exchanged, or `None`.
    pub last_event_time: Option<DateTime<Utc>>,
}

/// Minimal entity query the trait exposes. Backends may offer richer
/// entity interfaces; the trait only guarantees filter-by-type.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EntityQuery {
    /// Filter by entity type.
    pub entity_type: Option<String>,
    /// Filter by entity name substring (case-sensitive, LIKE-style).
    pub name_contains: Option<String>,
    /// Cap on the number of results. `None` means no cap.
    pub limit: Option<usize>,
}

/// Minimal entity row as the trait surface exposes. Concrete backends
/// may carry more fields internally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityRow {
    /// Entity unique id (free-form string).
    pub id: String,
    /// Entity type (e.g. "person", "repo").
    pub entity_type: String,
    /// Human-readable name.
    pub name: String,
    /// Arbitrary JSON properties.
    pub properties: serde_json::Value,
    /// The event id from which this entity was derived.
    pub source_event_id: String,
}

/// Minimal relationship row as the trait surface exposes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationshipRow {
    /// Relationship unique id.
    pub id: String,
    /// Source entity id.
    pub from_entity_id: String,
    /// Target entity id.
    pub to_entity_id: String,
    /// Relationship label (e.g. "authored", "reviewed").
    pub relationship_type: String,
    /// Arbitrary JSON properties.
    pub properties: serde_json::Value,
    /// Source event id.
    pub source_event_id: String,
}

/// A scored vector-search result.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    /// The event id whose embedding matched.
    pub event_id: String,
    /// Distance (lower is closer) or score depending on backend.
    pub score: f32,
}

/// Per-backend storage interface for ctxd.
///
/// Every method is async. The trait is `Send + Sync` so callers can share
/// `Arc<dyn Store>` across tasks. Default implementations are intentionally
/// omitted — we want every backend to make explicit choices about each
/// method so we can catch missed behavior in the shared conformance tests.
///
/// ## Error model
///
/// All fallible methods return [`StoreError`]. Backends wrap native errors
/// with [`StoreError::backend`]. Callers that need to special-case should
/// match on the variant.
#[async_trait]
pub trait Store: Send + Sync + std::fmt::Debug {
    /// Append an event to the log.
    ///
    /// The backend fills in `predecessorhash` (if a prior event for the
    /// same subject exists) and `signature` (if a signing key has been
    /// installed out-of-band). Returns the stored event with those fields
    /// populated.
    async fn append(&self, event: Event) -> Result<Event, StoreError>;

    /// Read events for a subject, optionally recursive.
    async fn read(&self, subject: &Subject, recursive: bool) -> Result<Vec<Event>, StoreError>;

    /// Read events for a subject at a point in time (time <= as_of),
    /// optionally recursive.
    async fn read_at(
        &self,
        subject: &Subject,
        as_of: DateTime<Utc>,
        recursive: bool,
    ) -> Result<Vec<Event>, StoreError>;

    /// List distinct subjects, optionally under a prefix.
    async fn subjects(
        &self,
        prefix: Option<&Subject>,
        recursive: bool,
    ) -> Result<Vec<String>, StoreError>;

    /// Full-text search over events.
    async fn search(&self, query: &str, limit: Option<usize>) -> Result<Vec<Event>, StoreError>;

    /// Return the latest KV-view value for a subject.
    async fn kv_get(&self, subject: &str) -> Result<Option<serde_json::Value>, StoreError>;

    /// Return the KV-view value for a subject as of the given timestamp.
    async fn kv_get_at(
        &self,
        subject: &str,
        as_of: DateTime<Utc>,
    ) -> Result<Option<serde_json::Value>, StoreError>;

    /// Query entities, filtered by the supplied [`EntityQuery`].
    async fn entities_query(&self, q: &EntityQuery) -> Result<Vec<EntityRow>, StoreError>;

    /// Return relationships for an entity (both incoming and outgoing).
    async fn relationships_for(
        &self,
        entity_id: &str,
    ) -> Result<Vec<(RelationshipRow, EntityRow)>, StoreError>;

    /// Register a peer. Idempotent on `peer_id`.
    async fn peer_add(&self, peer: Peer) -> Result<(), StoreError>;

    /// Return all registered peers.
    async fn peer_list(&self) -> Result<Vec<Peer>, StoreError>;

    /// Remove a peer by id. Returns `Ok(())` whether or not it existed.
    async fn peer_remove(&self, peer_id: &str) -> Result<(), StoreError>;

    /// Upsert a replication cursor for a peer + subject pattern.
    async fn peer_cursor_set(&self, cursor: PeerCursor) -> Result<(), StoreError>;

    /// Fetch a replication cursor. Returns `None` if no cursor exists.
    async fn peer_cursor_get(
        &self,
        peer_id: &str,
        subject_pattern: &str,
    ) -> Result<Option<PeerCursor>, StoreError>;

    /// Revoke a token by its biscuit token id.
    async fn revoke_token(&self, token_id: &str) -> Result<(), StoreError>;

    /// Check whether a token has been revoked.
    async fn is_token_revoked(&self, token_id: &str) -> Result<bool, StoreError>;

    /// Upsert a vector embedding for an event.
    async fn vector_upsert(
        &self,
        event_id: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<(), StoreError>;

    /// Return the `k` nearest embeddings to `query`. Backends that don't
    /// implement vector search should return an empty vec.
    async fn vector_search(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<VectorSearchResult>, StoreError>;
}
