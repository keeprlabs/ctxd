//! Vector index materialized view.
//!
//! # Status: Stub for v0.1
//!
//! This module will provide HNSW-based vector similarity search over event data.
//! Embeddings are pluggable — the user configures an embedding provider, and ctxd
//! generates embeddings on ingest and indexes them for nearest-neighbor queries.
//!
//! Candidate crates for v0.2 implementation:
//! - `hnsw_rs` — pure Rust HNSW implementation
//! - `instant-distance` — another pure Rust option, simpler API
//!
//! The vector view will be rebuilt from the event log like all other views.

#[allow(dead_code)]
/// Configuration for the vector index.
pub struct VectorViewConfig {
    /// Dimensionality of embedding vectors.
    pub dimensions: usize,
    /// Maximum number of connections per node in the HNSW graph.
    pub max_connections: usize,
    /// Size of the dynamic candidate list during construction.
    pub ef_construction: usize,
}

#[allow(dead_code)]
impl VectorViewConfig {
    /// Default configuration for 1536-dimensional embeddings (OpenAI ada-002 / text-embedding-3-small).
    pub fn default_openai() -> Self {
        Self {
            dimensions: 1536,
            max_connections: 16,
            ef_construction: 200,
        }
    }
}
