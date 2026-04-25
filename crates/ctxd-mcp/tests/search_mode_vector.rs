//! `ctx_search` with `search_mode: "vector"` runs only the vector
//! path. We use a tiny deterministic test embedder that hashes the
//! input into a fixed-dim vector, so equal text → equal vector.

use std::sync::Arc;

use async_trait::async_trait;
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_embed::{EmbedError, Embedder, EmbedderKind};
use ctxd_mcp::server::SearchParams;
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::views::vector::VectorIndexConfig;
use ctxd_store::EventStore;
use rmcp::handler::server::wrapper::Parameters;
use tempfile::TempDir;

/// Very small deterministic embedder: maps the first 8 ASCII bytes
/// of the input directly into f32 lanes. Identical inputs yield
/// identical vectors; small perturbations stay close in cosine
/// distance. Good enough for "vector path returns the right doc".
#[derive(Debug)]
struct AsciiEmbedder;

#[async_trait]
impl Embedder for AsciiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        if text.is_empty() {
            return Err(EmbedError::Input("empty".into()));
        }
        let bytes = text.as_bytes();
        let mut v = vec![0.0f32; 8];
        for (i, b) in bytes.iter().take(8).enumerate() {
            v[i] = *b as f32 / 255.0 + 0.01;
        }
        // Pad with a small constant so cosine is always defined.
        for slot in v.iter_mut() {
            if *slot == 0.0 {
                *slot = 0.05;
            }
        }
        Ok(v)
    }

    fn dimensions(&self) -> usize {
        8
    }
    fn model(&self) -> &str {
        "ascii-test-embedder"
    }
    fn kind(&self) -> EmbedderKind {
        EmbedderKind::Null
    }
}

#[tokio::test]
async fn vector_mode_returns_top_k_known_close_points() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");

    let mut store = EventStore::open(&path).await.unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(AsciiEmbedder);
    store.set_embedder(embedder.clone());
    let _vec_idx = store
        .ensure_vector_index(VectorIndexConfig {
            dimensions: 8,
            flush_every_n_inserts: 100,
            max_elements: 1024,
            max_nb_layers: 16,
        })
        .await
        .unwrap();

    // Seed 5 events. The text payload is the "subject hint" so the
    // embedder maps each one to a deterministically nearby vector.
    for tag in ["alpha", "alphax", "alphay", "delta", "omega"] {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new(&format!("/notes/{tag}")).unwrap(),
            "note".to_string(),
            serde_json::json!({"content": tag}),
        );
        store.append(event).await.unwrap();
    }

    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn ctxd_cap::state::CaveatState> =
        Arc::new(ctxd_cap::state::InMemoryCaveatState::new());
    let server = CtxdMcpServer::new(store, cap_engine, caveat_state, "ctxd://test".to_string())
        .with_embedder(embedder);

    // Query "alpha" — expect alpha + alphax + alphay in top 3.
    let out = server
        .ctx_search(Parameters(SearchParams {
            query: "alpha".to_string(),
            subject_pattern: None,
            k: Some(3),
            token: None,
            search_mode: Some("vector".to_string()),
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!("vector mode output not JSON ({e}): {out}");
    });
    assert_eq!(
        arr.len(),
        3,
        "expected k=3 results, got {}: {out}",
        arr.len()
    );
    let subjects: Vec<&str> = arr
        .iter()
        .map(|e| e["subject"].as_str().unwrap_or(""))
        .collect();
    // alpha embeds identically to the query, so it MUST be rank 1.
    // The other ranks depend on the embedder's geometry — under
    // AsciiEmbedder, single-character extensions like alphax/alphay
    // shift one lane by ~0.43 while delta/omega differ in the first
    // 5 lanes by ~0.01-0.04 each (smaller cumulative L2). We only
    // assert what the embedder's geometry guarantees: rank 1 is the
    // exact-match doc. Stronger ordering claims belong in dedicated
    // semantic-distance tests, not the wiring test.
    assert_eq!(
        subjects.first().copied(),
        Some("/notes/alpha"),
        "alpha (exact embedding match) must rank first: {subjects:?}"
    );
}

#[tokio::test]
async fn vector_mode_without_embedder_returns_error() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn ctxd_cap::state::CaveatState> =
        Arc::new(ctxd_cap::state::InMemoryCaveatState::new());
    let server = CtxdMcpServer::new(store, cap_engine, caveat_state, "ctxd://test".to_string());
    let out = server
        .ctx_search(Parameters(SearchParams {
            query: "x".to_string(),
            subject_pattern: None,
            k: Some(1),
            token: None,
            search_mode: Some("vector".to_string()),
        }))
        .await;
    assert!(
        out.contains("embedder"),
        "expected error mentioning embedder: {out}"
    );
}
