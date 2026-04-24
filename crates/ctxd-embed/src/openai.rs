//! OpenAI-hosted embedder.
//!
//! Wraps `POST {base_url}/embeddings` with retry-after-aware
//! exponential backoff. Honors the OpenAI 256-input-per-request
//! batch cap by chunking large `embed_batch` calls.
//!
//! API keys are NEVER included in tracing output or error messages —
//! they're held privately on the struct, redacted on Debug, and never
//! interpolated into logs.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::retry::{backoff, MAX_ATTEMPTS};
use crate::{EmbedError, Embedder, EmbedderKind};

/// OpenAI's hard cap on inputs per `/v1/embeddings` request.
const MAX_BATCH_PER_REQUEST: usize = 256;

/// Default endpoint base URL.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Default model — the small 3rd-generation embedding model. 1536-dim.
const DEFAULT_MODEL: &str = "text-embedding-3-small";

/// Default dimensions for `text-embedding-3-small`. The OpenAI API
/// supports a `dimensions` request parameter that truncates the
/// returned vector, but we default to the model's native size.
const DEFAULT_DIMS: usize = 1536;

/// Configuration for [`OpenAiEmbedder`]. Build via
/// [`OpenAiEmbedder::new`] or [`OpenAiEmbedder::builder`].
#[derive(Clone)]
pub struct OpenAiConfig {
    api_key: String,
    base_url: String,
    model: String,
    dimensions: usize,
}

impl std::fmt::Debug for OpenAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiConfig")
            .field("api_key", &"***redacted***")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

/// Real OpenAI embedder behind `feature = "openai"`.
///
/// Cheap to clone (`reqwest::Client` is `Arc` internally). Hold an
/// `Arc<OpenAiEmbedder>` or clone freely.
#[derive(Clone)]
pub struct OpenAiEmbedder {
    cfg: OpenAiConfig,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiEmbedder")
            .field("cfg", &self.cfg)
            .finish()
    }
}

/// Builder for [`OpenAiEmbedder`]. Use this when you want to override
/// the model, endpoint, or expected dimensions.
pub struct OpenAiBuilder {
    api_key: Option<String>,
    base_url: String,
    model: String,
    dimensions: usize,
}

impl OpenAiBuilder {
    /// Set the API key (read from `OPENAI_API_KEY` if unset).
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the API base URL. Useful for proxies, Azure
    /// OpenAI, or wiremock fixtures in tests.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the model. Default: `text-embedding-3-small`.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the expected dimensionality. Default depends on model.
    pub fn dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims.max(1);
        self
    }

    /// Construct the embedder. Fails if no API key is found.
    pub fn build(self) -> Result<OpenAiEmbedder, EmbedError> {
        let api_key = self
            .api_key
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| {
                EmbedError::Input("OPENAI_API_KEY not set and no api_key configured".to_string())
            })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| EmbedError::Network(format!("reqwest client init: {e}")))?;
        Ok(OpenAiEmbedder {
            cfg: OpenAiConfig {
                api_key,
                base_url: self.base_url,
                model: self.model,
                dimensions: self.dimensions,
            },
            client,
        })
    }
}

impl OpenAiEmbedder {
    /// Create with defaults (`text-embedding-3-small`, 1536 dims, env API key).
    pub fn new() -> Result<Self, EmbedError> {
        Self::builder().build()
    }

    /// Start a builder for fine-grained config.
    pub fn builder() -> OpenAiBuilder {
        OpenAiBuilder {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            dimensions: DEFAULT_DIMS,
        }
    }

    /// Borrow the model name.
    pub fn model_name(&self) -> &str {
        &self.cfg.model
    }

    /// Borrow the base URL — exposed for testing assertions.
    pub fn base_url(&self) -> &str {
        &self.cfg.base_url
    }

    /// Submit one batch (already <= 256 inputs) with retry/backoff.
    async fn request_one_chunk(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        if inputs.len() > MAX_BATCH_PER_REQUEST {
            return Err(EmbedError::Input(format!(
                "batch size {} exceeds OpenAI cap {MAX_BATCH_PER_REQUEST}",
                inputs.len()
            )));
        }

        let url = format!("{}/embeddings", self.cfg.base_url.trim_end_matches('/'));
        let body = OpenAiRequest {
            model: &self.cfg.model,
            input: inputs,
        };

        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.cfg.api_key)
                .json(&body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    // Network errors are retryable up to MAX_ATTEMPTS.
                    if attempt >= MAX_ATTEMPTS {
                        return Err(EmbedError::Network(format!(
                            "openai request failed after {attempt} attempts"
                        )));
                    }
                    tracing::warn!(attempt, error = %e, "openai network error; retrying");
                    tokio::time::sleep(backoff(attempt, None)).await;
                    continue;
                }
            };

            let status = resp.status();
            if status.is_success() {
                let parsed: OpenAiResponse = resp.json().await.map_err(|e| {
                    EmbedError::Response(format!("openai response parse failed: {e}"))
                })?;
                let mut out = Vec::with_capacity(parsed.data.len());
                // OpenAI returns the inputs in request order with an
                // explicit `index` field. Sort by index to be safe.
                let mut sorted = parsed.data;
                sorted.sort_by_key(|d| d.index);
                for d in sorted {
                    out.push(d.embedding);
                }
                if out.len() != inputs.len() {
                    return Err(EmbedError::Response(format!(
                        "openai returned {} embeddings for {} inputs",
                        out.len(),
                        inputs.len()
                    )));
                }
                return Ok(out);
            }

            // Non-2xx. 429 is always retryable; 5xx is retryable; 4xx
            // (other) is a hard error — usually a key or model problem.
            let retry_after = parse_retry_after(resp.headers().get("retry-after"));
            let body_text = resp.text().await.unwrap_or_default();
            // Redact: never log the full body if it could contain echoed credentials.
            let snippet = body_text.chars().take(256).collect::<String>();

            if status.as_u16() == 429 || status.is_server_error() {
                if attempt >= MAX_ATTEMPTS {
                    return Err(EmbedError::Network(format!(
                        "openai status {status} after {attempt} attempts"
                    )));
                }
                tracing::warn!(
                    attempt,
                    status = %status,
                    "openai retryable status; backing off"
                );
                tokio::time::sleep(backoff(attempt, retry_after)).await;
                continue;
            }

            // Non-retryable. Surface the status + a short snippet.
            return Err(EmbedError::Response(format!(
                "openai status {status}: {snippet}"
            )));
        }
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        if text.is_empty() {
            return Err(EmbedError::Input("empty text".to_string()));
        }
        let mut v = self.request_one_chunk(&[text]).await?;
        v.pop()
            .ok_or_else(|| EmbedError::Response("empty embedding result".to_string()))
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH_PER_REQUEST) {
            let part = self.request_one_chunk(chunk).await?;
            out.extend(part);
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
        EmbedderKind::OpenAi
    }
}

#[derive(Serialize)]
struct OpenAiRequest<'a> {
    model: &'a str,
    input: &'a [&'a str],
}

#[derive(Deserialize)]
struct OpenAiResponse {
    data: Vec<OpenAiEmbedding>,
}

#[derive(Deserialize)]
struct OpenAiEmbedding {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

fn parse_retry_after(h: Option<&reqwest::header::HeaderValue>) -> Option<Duration> {
    let v = h?.to_str().ok()?;
    // OpenAI returns Retry-After as fractional seconds.
    if let Ok(secs) = v.parse::<f64>() {
        if secs.is_finite() && secs >= 0.0 {
            return Some(Duration::from_millis((secs * 1000.0) as u64));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_api_key() {
        let cfg = OpenAiConfig {
            api_key: "sk-supersecret-abcdef".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            model: "text-embedding-3-small".to_string(),
            dimensions: 1536,
        };
        let s = format!("{cfg:?}");
        assert!(!s.contains("supersecret"), "api key leaked in Debug: {s}");
        assert!(s.contains("redacted"));
    }

    #[test]
    fn parse_retry_after_handles_floats() {
        // Build a HeaderValue manually
        let v = reqwest::header::HeaderValue::from_static("0.5");
        let d = parse_retry_after(Some(&v)).unwrap();
        assert_eq!(d, Duration::from_millis(500));
    }
}
