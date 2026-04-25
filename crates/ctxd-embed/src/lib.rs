//! Embedder trait and default impls for ctxd.
//!
//! Phase 4A of v0.3 introduces a pluggable [`Embedder`] so that ctxd
//! can produce vector embeddings for events (used by `ctx_search`'s
//! vector / hybrid modes once Phase 4B lands).
//!
//! This crate ships:
//!
//! - [`Embedder`] — the async trait every backend implements.
//! - [`NullEmbedder`] — the default, returns a deterministic all-zero
//!   vector of fixed dimensionality. Useful as a sentinel: callers can
//!   detect "no real embedder configured" without branching on
//!   `Option<Box<dyn Embedder>>`.
//! - [`EmbedderKind`] — an enum describing which backend is active,
//!   for logging and human-readable config output.
//!
//! Real backends:
//!
//! - [`openai::OpenAiEmbedder`] (behind `feature = "openai"`) — talks
//!   to OpenAI's `/v1/embeddings` endpoint with retry-after-aware
//!   exponential backoff and the OpenAI 256-input batch cap.
//! - [`ollama::OllamaEmbedder`] (behind `feature = "ollama"`) —
//!   talks to a locally-running Ollama daemon over its
//!   `/api/embeddings` endpoint. No auth, no batch endpoint —
//!   batched calls fan out per-text in-process.

use async_trait::async_trait;

#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "ollama")]
pub mod ollama;

#[cfg(any(feature = "openai", feature = "ollama"))]
mod retry;

/// Errors surfaced by an [`Embedder`].
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// Backend-specific HTTP or network error.
    #[error("embedder network error: {0}")]
    Network(String),
    /// Backend returned a malformed response (missing fields, wrong shape).
    #[error("embedder response error: {0}")]
    Response(String),
    /// Input was invalid (e.g. empty text, too many tokens).
    #[error("embedder input error: {0}")]
    Input(String),
    /// Catch-all for implementations that don't need fine-grained variants.
    #[error("embedder error: {0}")]
    Other(String),
}

/// A name for the backend, useful for tracing + config output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedderKind {
    /// The [`NullEmbedder`] — returns zero vectors.
    Null,
    /// An OpenAI-hosted embedder.
    OpenAi,
    /// A local Ollama-hosted embedder.
    Ollama,
}

impl std::fmt::Display for EmbedderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedderKind::Null => f.write_str("null"),
            EmbedderKind::OpenAi => f.write_str("openai"),
            EmbedderKind::Ollama => f.write_str("ollama"),
        }
    }
}

/// Async trait for text -> vector embedding.
///
/// Implementations are expected to be cheap to clone (typically a
/// handle around a `reqwest::Client` or similar). Callers typically
/// hold `Arc<dyn Embedder>`.
#[async_trait]
pub trait Embedder: Send + Sync + std::fmt::Debug {
    /// Embed a single text. Returns a vector of length
    /// `self.dimensions()`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError>;

    /// Batch variant — backends that can process multiple texts in one
    /// round-trip should override this; the default falls back to
    /// per-text calls.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }

    /// Dimensionality of emitted vectors.
    fn dimensions(&self) -> usize;

    /// Identifier for the model in use, e.g. `"text-embedding-3-small"`.
    fn model(&self) -> &str;

    /// Backend kind, for logs + config output.
    fn kind(&self) -> EmbedderKind;
}

/// The default embedder — returns a fixed-length all-zero vector.
///
/// Exists so call sites can hold an `Arc<dyn Embedder>` unconditionally.
/// `vector_search` against zero vectors produces uniform cosine
/// distance, which is a safe no-op.
#[derive(Debug, Clone)]
pub struct NullEmbedder {
    dims: usize,
}

impl NullEmbedder {
    /// Create a null embedder producing vectors of length `dims`.
    /// `dims` must be > 0; if zero is passed we clamp to 1 so downstream
    /// vector stores don't panic on empty vectors.
    pub fn new(dims: usize) -> Self {
        Self { dims: dims.max(1) }
    }
}

impl Default for NullEmbedder {
    fn default() -> Self {
        // 384 mirrors all-MiniLM-L6-v2, a common small open model;
        // chosen so a persisted index happens to be dimensionally
        // compatible with that model.
        Self::new(384)
    }
}

#[async_trait]
impl Embedder for NullEmbedder {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(vec![0.0; self.dims])
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model(&self) -> &str {
        "null-embedder"
    }

    fn kind(&self) -> EmbedderKind {
        EmbedderKind::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_embedder_emits_zero_vectors() {
        let e = NullEmbedder::new(16);
        let v = e.embed("hello world").await.unwrap();
        assert_eq!(v.len(), 16);
        assert!(v.iter().all(|f| *f == 0.0));
    }

    #[tokio::test]
    async fn null_embedder_clamps_zero_dims() {
        let e = NullEmbedder::new(0);
        let v = e.embed("").await.unwrap();
        assert_eq!(v.len(), 1, "dims clamped to 1 to avoid empty vectors");
    }

    #[tokio::test]
    async fn null_embedder_batch_length_matches_input() {
        let e = NullEmbedder::new(8);
        let out = e.embed_batch(&["a", "b", "c"]).await.unwrap();
        assert_eq!(out.len(), 3);
        for v in out {
            assert_eq!(v.len(), 8);
        }
    }

    #[test]
    fn embedder_kind_display_matches_cli_flag() {
        assert_eq!(EmbedderKind::Null.to_string(), "null");
        assert_eq!(EmbedderKind::OpenAi.to_string(), "openai");
        assert_eq!(EmbedderKind::Ollama.to_string(), "ollama");
    }
}
