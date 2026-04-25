//! Ollama-hosted embedder.
//!
//! Wraps `POST {base_url}/api/embeddings` for a locally-running
//! Ollama daemon. No auth. Ollama doesn't (yet) support multi-input
//! batching, so [`OllamaEmbedder::embed_batch`] fans out per-text
//! sequentially. We keep retry-after / exponential-backoff handling
//! consistent with [`crate::openai`].

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::retry::{backoff, MAX_ATTEMPTS};
use crate::{EmbedError, Embedder, EmbedderKind};

/// Default Ollama daemon URL.
const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Default model — `nomic-embed-text` is 768-dim and ships in
/// `ollama pull nomic-embed-text`.
const DEFAULT_MODEL: &str = "nomic-embed-text";

/// Default dimensionality for `nomic-embed-text`.
const DEFAULT_DIMS: usize = 768;

/// Configuration for [`OllamaEmbedder`].
#[derive(Clone, Debug)]
pub struct OllamaConfig {
    base_url: String,
    model: String,
    dimensions: usize,
}

/// Real Ollama embedder behind `feature = "ollama"`.
#[derive(Clone, Debug)]
pub struct OllamaEmbedder {
    cfg: OllamaConfig,
    client: reqwest::Client,
}

/// Builder for [`OllamaEmbedder`].
pub struct OllamaBuilder {
    base_url: String,
    model: String,
    dimensions: usize,
}

impl OllamaBuilder {
    /// Override the daemon URL. Default: `http://localhost:11434`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the model. Default: `nomic-embed-text`.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the expected dimensionality. Default: 768.
    pub fn dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims.max(1);
        self
    }

    /// Construct the embedder.
    pub fn build(self) -> Result<OllamaEmbedder, EmbedError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| EmbedError::Network(format!("reqwest client init: {e}")))?;
        Ok(OllamaEmbedder {
            cfg: OllamaConfig {
                base_url: self.base_url,
                model: self.model,
                dimensions: self.dimensions,
            },
            client,
        })
    }
}

impl OllamaEmbedder {
    /// Create with defaults (`nomic-embed-text`, 768 dims, localhost daemon).
    pub fn new() -> Result<Self, EmbedError> {
        Self::builder().build()
    }

    /// Start a builder for fine-grained config.
    pub fn builder() -> OllamaBuilder {
        OllamaBuilder {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            dimensions: DEFAULT_DIMS,
        }
    }

    /// Borrow the configured base URL — exposed for tests.
    pub fn base_url(&self) -> &str {
        &self.cfg.base_url
    }

    /// Submit a single-text embedding request with retry/backoff.
    async fn request_one(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        if text.is_empty() {
            return Err(EmbedError::Input("empty text".to_string()));
        }
        let url = format!("{}/api/embeddings", self.cfg.base_url.trim_end_matches('/'));
        let body = OllamaRequest {
            model: &self.cfg.model,
            prompt: text,
        };

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let resp = self.client.post(&url).json(&body).send().await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(EmbedError::Network(format!(
                            "ollama request failed after {attempt} attempts"
                        )));
                    }
                    tracing::warn!(attempt, error = %e, "ollama network error; retrying");
                    tokio::time::sleep(backoff(attempt, None)).await;
                    continue;
                }
            };
            let status = resp.status();
            if status.is_success() {
                let parsed: OllamaResponse = resp.json().await.map_err(|e| {
                    EmbedError::Response(format!("ollama response parse failed: {e}"))
                })?;
                if parsed.embedding.is_empty() {
                    return Err(EmbedError::Response(
                        "ollama returned empty embedding".to_string(),
                    ));
                }
                return Ok(parsed.embedding);
            }
            // Ollama uses standard HTTP semantics. 5xx + 429 retryable.
            if status.as_u16() == 429 || status.is_server_error() {
                if attempt >= MAX_ATTEMPTS {
                    return Err(EmbedError::Network(format!(
                        "ollama status {status} after {attempt} attempts"
                    )));
                }
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);
                tracing::warn!(
                    attempt,
                    status = %status,
                    "ollama retryable status; backing off"
                );
                tokio::time::sleep(backoff(attempt, retry_after)).await;
                continue;
            }
            let body_text = resp.text().await.unwrap_or_default();
            let snippet = body_text.chars().take(256).collect::<String>();
            return Err(EmbedError::Response(format!(
                "ollama status {status}: {snippet}"
            )));
        }
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.request_one(text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.request_one(t).await?);
        }
        Ok(out)
    }

    fn dimensions(&self) -> usize {
        self.cfg.dimensions
    }

    fn model(&self) -> &str {
        &self.cfg.model
    }

    fn kind(&self) -> EmbedderKind {
        EmbedderKind::Ollama
    }
}

#[derive(Serialize)]
struct OllamaRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Deserialize)]
struct OllamaResponse {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        let b = OllamaEmbedder::builder();
        assert_eq!(b.base_url, DEFAULT_BASE_URL);
        assert_eq!(b.model, DEFAULT_MODEL);
        assert_eq!(b.dimensions, DEFAULT_DIMS);
    }
}
