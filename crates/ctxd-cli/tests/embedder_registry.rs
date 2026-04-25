//! Tests the CLI embedder registry: maps the parsed `--embedder`
//! choice + flags onto the right `Arc<dyn Embedder>`.

use ctxd_cli::embedder::{build_embedder, EmbedderChoice, EmbedderOpts};
use ctxd_embed::EmbedderKind;

#[test]
fn null_choice_constructs_null_embedder() {
    let e = build_embedder(
        EmbedderChoice::Null,
        EmbedderOpts {
            dimensions: Some(8),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(e.kind(), EmbedderKind::Null);
    assert_eq!(e.dimensions(), 8);
    assert_eq!(e.model(), "null-embedder");
}

#[cfg(feature = "openai")]
#[test]
fn openai_choice_constructs_openai_embedder() {
    let e = build_embedder(
        EmbedderChoice::OpenAi,
        EmbedderOpts {
            api_key: Some("test-key".to_string()),
            url: Some("http://localhost:1".to_string()),
            model: Some("custom-model".to_string()),
            dimensions: Some(1024),
        },
    )
    .unwrap();
    assert_eq!(e.kind(), EmbedderKind::OpenAi);
    assert_eq!(e.model(), "custom-model");
    assert_eq!(e.dimensions(), 1024);
}

#[cfg(feature = "ollama")]
#[test]
fn ollama_choice_constructs_ollama_embedder() {
    let e = build_embedder(
        EmbedderChoice::Ollama,
        EmbedderOpts {
            url: Some("http://localhost:1".to_string()),
            model: Some("nomic-embed-text".to_string()),
            dimensions: Some(768),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(e.kind(), EmbedderKind::Ollama);
    assert_eq!(e.dimensions(), 768);
}

#[test]
fn parse_unknown_provider_errors() {
    assert!(EmbedderChoice::parse("bigml").is_err());
}

#[test]
fn parse_known_providers() {
    assert_eq!(EmbedderChoice::parse("null").unwrap(), EmbedderChoice::Null);
    assert_eq!(
        EmbedderChoice::parse("openai").unwrap(),
        EmbedderChoice::OpenAi
    );
    assert_eq!(
        EmbedderChoice::parse("ollama").unwrap(),
        EmbedderChoice::Ollama
    );
}
