//! Embedder registry — turns CLI flags into an `Arc<dyn Embedder>`.
//!
//! This is the single place that knows how to construct each
//! provider, so binaries just pass [`EmbedderOpts`] in and get back a
//! ready-to-use trait object. Construction errors return early to
//! `serve` so the operator sees the misconfiguration immediately
//! instead of failing later on the first auto-embed call.

use std::sync::Arc;

use ctxd_embed::{EmbedError, Embedder, NullEmbedder};

/// Which provider was selected on the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedderChoice {
    /// All-zero vectors — the default. Useful for tests + dev,
    /// preserves the `Arc<dyn Embedder>` contract without any IO.
    Null,
    /// Real OpenAI provider — needs `feature = "openai"`.
    OpenAi,
    /// Real Ollama provider — needs `feature = "ollama"`.
    Ollama,
}

impl EmbedderChoice {
    /// Parse one of `null`, `openai`, `ollama` (case-insensitive).
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "null" => Ok(Self::Null),
            "openai" => Ok(Self::OpenAi),
            "ollama" => Ok(Self::Ollama),
            other => Err(format!(
                "unknown embedder '{other}' (expected null|openai|ollama)"
            )),
        }
    }
}

/// CLI options that map onto an embedder.
#[derive(Debug, Clone, Default)]
pub struct EmbedderOpts {
    /// Override the model. If `None`, the provider default is used.
    pub model: Option<String>,
    /// Override the API base URL.
    pub url: Option<String>,
    /// API key (OpenAI). Falls back to `OPENAI_API_KEY` env if `None`.
    pub api_key: Option<String>,
    /// Override the expected dimensionality. Required for `null` to
    /// match a persisted index.
    pub dimensions: Option<usize>,
}

/// Build an `Arc<dyn Embedder>` from [`EmbedderChoice`] + [`EmbedderOpts`].
///
/// Returns [`EmbedError::Other`] if a provider is selected whose
/// feature flag wasn't enabled at compile time.
#[allow(unused_variables, unused_mut)]
pub fn build_embedder(
    choice: EmbedderChoice,
    opts: EmbedderOpts,
) -> Result<Arc<dyn Embedder>, EmbedError> {
    match choice {
        EmbedderChoice::Null => {
            let dims = opts.dimensions.unwrap_or(384);
            Ok(Arc::new(NullEmbedder::new(dims)))
        }
        EmbedderChoice::OpenAi => {
            #[cfg(feature = "openai")]
            {
                let mut b = ctxd_embed::openai::OpenAiEmbedder::builder();
                if let Some(k) = opts.api_key {
                    b = b.api_key(k);
                }
                if let Some(u) = opts.url {
                    b = b.base_url(u);
                }
                if let Some(m) = opts.model {
                    b = b.model(m);
                }
                if let Some(d) = opts.dimensions {
                    b = b.dimensions(d);
                }
                let e = b.build()?;
                Ok(Arc::new(e))
            }
            #[cfg(not(feature = "openai"))]
            {
                Err(EmbedError::Other(
                    "openai feature not enabled in this build".to_string(),
                ))
            }
        }
        EmbedderChoice::Ollama => {
            #[cfg(feature = "ollama")]
            {
                let mut b = ctxd_embed::ollama::OllamaEmbedder::builder();
                if let Some(u) = opts.url {
                    b = b.base_url(u);
                }
                if let Some(m) = opts.model {
                    b = b.model(m);
                }
                if let Some(d) = opts.dimensions {
                    b = b.dimensions(d);
                }
                let e = b.build()?;
                Ok(Arc::new(e))
            }
            #[cfg(not(feature = "ollama"))]
            {
                Err(EmbedError::Other(
                    "ollama feature not enabled in this build".to_string(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_choices() {
        assert_eq!(EmbedderChoice::parse("null").unwrap(), EmbedderChoice::Null);
        assert_eq!(
            EmbedderChoice::parse("OPENAI").unwrap(),
            EmbedderChoice::OpenAi
        );
        assert!(EmbedderChoice::parse("bogus").is_err());
    }

    #[test]
    fn null_choice_constructs_null_embedder() {
        let e = build_embedder(
            EmbedderChoice::Null,
            EmbedderOpts {
                dimensions: Some(16),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(e.dimensions(), 16);
        assert_eq!(e.kind(), ctxd_embed::EmbedderKind::Null);
    }
}
