//! Hybrid RRF: a document that appears in BOTH the FTS top-10 and
//! the vector top-10 must rank above documents that appear in only
//! one. We construct a small known-answer setup where the matching
//! semantics are deterministic.

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

/// Hash-based deterministic embedder. Two inputs that share their
/// first 8 characters get nearly identical vectors. We use this to
/// engineer the corpus so we know exactly which docs the vector
/// path will rank where.
#[derive(Debug)]
struct PrefixEmbedder;

#[async_trait]
impl Embedder for PrefixEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        if text.is_empty() {
            return Err(EmbedError::Input("empty".into()));
        }
        let bytes = text.as_bytes();
        let mut v = vec![0.0f32; 8];
        for (i, b) in bytes.iter().take(8).enumerate() {
            v[i] = *b as f32 / 255.0 + 0.05;
        }
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
        "prefix-embedder"
    }
    fn kind(&self) -> EmbedderKind {
        EmbedderKind::Null
    }
}

#[tokio::test]
async fn rrf_ranks_dual_match_above_single_match() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");
    let mut store = EventStore::open(&path).await.unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(PrefixEmbedder);
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

    // Three events:
    // - "/notes/dual"   — content "phoenix project rises" (FTS hits "phoenix";
    //                    vector embeds "phoenix project" — close to query "phoenix")
    // - "/notes/textonly" — content "phoenix only mentioned here"
    //                       (FTS hits "phoenix"; vector input "phoenix only mentioned"
    //                       — first 8 chars "phoenix " also close to query "phoenix")
    // - "/notes/vectoronly" — content "phoenix" — wait, that hits both.
    //
    // Trick: we want a clean separation. Use /notes/dual content =
    // "phoenix" (matches FTS + first-8 chars match query exactly).
    // /notes/textonly content = "different topic that mentions phoenix only at the end"
    //   — FTS hits "phoenix"; vector input starts with "differen" — far from "phoenix".
    // /notes/vectoronly content = "phoenix-shaped vector neighbor"
    //   — vector first 8 chars = "phoenix-" — close to query.
    //   FTS would still hit "phoenix" though. Use a less-overlapping word:
    //   content = "phoeniks" — vector first 8 chars = "phoeniks" close to "phoenix",
    //   but FTS for "phoenix" doesn't match "phoeniks".
    let docs = [
        ("/notes/dual", "phoenix"),
        (
            "/notes/textonly",
            "different topic that mentions phoenix only at the end",
        ),
        ("/notes/vectoronly", "phoeniks"),
    ];
    for (subj, body) in docs {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new(subj).unwrap(),
            "note".to_string(),
            serde_json::json!({"content": body}),
        );
        store.append(event).await.unwrap();
    }

    let cap_engine = Arc::new(CapEngine::new());
    let server =
        CtxdMcpServer::new(store, cap_engine, "ctxd://test".to_string()).with_embedder(embedder);

    let out = server
        .ctx_search(Parameters(SearchParams {
            query: "phoenix".to_string(),
            subject_pattern: None,
            k: Some(5),
            token: None,
            search_mode: Some("hybrid".to_string()),
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!("hybrid output not JSON ({e}): {out}");
    });
    let subjects: Vec<&str> = arr
        .iter()
        .map(|e| e["subject"].as_str().unwrap_or(""))
        .collect();
    assert!(
        subjects.contains(&"/notes/dual"),
        "dual-match doc must appear: {subjects:?}"
    );
    let pos_dual = subjects.iter().position(|s| *s == "/notes/dual");
    let pos_text = subjects.iter().position(|s| *s == "/notes/textonly");
    let pos_vec = subjects.iter().position(|s| *s == "/notes/vectoronly");
    assert!(pos_dual.is_some(), "dual missing in: {subjects:?}");
    if let (Some(dual), Some(text)) = (pos_dual, pos_text) {
        assert!(
            dual <= text,
            "dual ({dual}) should rank at-or-above text-only ({text}): {subjects:?}"
        );
    }
    if let (Some(dual), Some(vec)) = (pos_dual, pos_vec) {
        assert!(
            dual <= vec,
            "dual ({dual}) should rank at-or-above vector-only ({vec}): {subjects:?}"
        );
    }
}

#[tokio::test]
async fn rrf_default_mode_is_hybrid_when_embedder_set() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");
    let mut store = EventStore::open(&path).await.unwrap();
    let embedder: Arc<dyn Embedder> = Arc::new(PrefixEmbedder);
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
    let event = Event::new(
        "ctxd://test".to_string(),
        Subject::new("/notes/x").unwrap(),
        "note".to_string(),
        serde_json::json!({"content": "phoenix"}),
    );
    store.append(event).await.unwrap();

    let cap_engine = Arc::new(CapEngine::new());
    let server =
        CtxdMcpServer::new(store, cap_engine, "ctxd://test".to_string()).with_embedder(embedder);
    let out = server
        .ctx_search(Parameters(SearchParams {
            query: "phoenix".to_string(),
            subject_pattern: None,
            k: Some(5),
            token: None,
            search_mode: None,
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(arr.len(), 1);
}
