//! `ctx_search` with `search_mode: "fts"` runs only the FTS path,
//! ignoring any embedder/vector index.

use std::sync::Arc;

use ctxd_cap::state::{CaveatState, InMemoryCaveatState};
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_embed::NullEmbedder;
use ctxd_mcp::server::SearchParams;
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use rmcp::handler::server::wrapper::Parameters;

async fn make_server_with_embedder() -> CtxdMcpServer {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(InMemoryCaveatState::new());
    CtxdMcpServer::new(store, cap_engine, caveat_state, "ctxd://test".to_string())
        .with_embedder(Arc::new(NullEmbedder::new(8)))
}

#[tokio::test]
async fn fts_mode_only_returns_text_matches() {
    let server = make_server_with_embedder().await;
    let store = server.store();

    // Two events: one matches "phoenix", the other doesn't.
    for (subj, body) in [
        ("/notes/a", "phoenix rises today"),
        ("/notes/b", "completely different topic"),
    ] {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new(subj).unwrap(),
            "note".to_string(),
            serde_json::json!({"content": body}),
        );
        store.append(event).await.unwrap();
    }

    let out = server
        .ctx_search(Parameters(SearchParams {
            query: "phoenix".to_string(),
            subject_pattern: None,
            k: Some(10),
            token: None,
            search_mode: Some("fts".to_string()),
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(arr.len(), 1, "expected single FTS match, got: {out}");
    assert_eq!(arr[0]["subject"], "/notes/a");
}

#[tokio::test]
async fn unknown_search_mode_returns_error() {
    let server = make_server_with_embedder().await;
    let out = server
        .ctx_search(Parameters(SearchParams {
            query: "x".to_string(),
            subject_pattern: None,
            k: Some(1),
            token: None,
            search_mode: Some("bogus".to_string()),
        }))
        .await;
    assert!(out.starts_with("error:"), "expected error, got: {out}");
}
