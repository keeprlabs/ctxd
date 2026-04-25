//! End-to-end tests for the v0.3 MCP tools (`ctx_entities`, `ctx_related`,
//! `ctx_timeline`). We exercise the tool method directly — the rmcp
//! macro-generated router adds JSON-RPC framing on top, but the tool's
//! user-visible contract is the `Parameters<...> -> String` shape we
//! assert against here.

use std::sync::Arc;

use chrono::TimeZone;
use ctxd_cap::state::{CaveatState, InMemoryCaveatState};
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_mcp::server::{EntitiesParams, RelatedParams, TimelineParams};
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use rmcp::handler::server::wrapper::Parameters;

async fn make_server() -> CtxdMcpServer {
    let store = EventStore::open_memory().await.expect("open store");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(InMemoryCaveatState::new());
    CtxdMcpServer::new(store, cap_engine, caveat_state, "ctxd://test".to_string())
}

fn store_of(server: &CtxdMcpServer) -> &EventStore {
    // Expose the store through a helper. The field is private; we clone
    // the public API instead of reaching inside.
    server.store()
}

// A tiny accessor so tests can seed the store without going through MCP.
// Added as an inherent method on CtxdMcpServer below.

#[tokio::test]
async fn ctx_timeline_returns_events_at_timestamp() {
    let server = make_server().await;
    let store = store_of(&server);
    let subject = Subject::new("/timeline/demo").unwrap();

    let t1 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap();
    let t2 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 11, 0, 0).unwrap();
    let t3 = chrono::Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap();

    for (i, t) in [(1, t1), (2, t2), (3, t3)] {
        let mut e = Event::new(
            "ctxd://test".to_string(),
            subject.clone(),
            "demo".to_string(),
            serde_json::json!({"v": i}),
        );
        e.time = t;
        store.append(e).await.unwrap();
    }

    let out = server
        .ctx_timeline(Parameters(TimelineParams {
            subject: "/timeline/demo".to_string(),
            as_of: t2.to_rfc3339(),
            recursive: false,
            token: None,
        }))
        .await;

    assert!(out.starts_with('['), "expected JSON array, got: {out}");
    let events: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(events.len(), 2, "t2 bound should yield events 1 and 2");
}

#[tokio::test]
async fn ctx_timeline_rejects_bad_as_of() {
    let server = make_server().await;
    let out = server
        .ctx_timeline(Parameters(TimelineParams {
            subject: "/whatever".to_string(),
            as_of: "not a timestamp".to_string(),
            recursive: false,
            token: None,
        }))
        .await;
    assert!(out.starts_with("error:"), "expected error, got: {out}");
}

#[tokio::test]
async fn ctx_entities_and_related_roundtrip() {
    let server = make_server().await;
    let store = store_of(&server);
    let graph = store.graph_view();

    use ctxd_store::views::graph::{Entity, Relationship};

    graph
        .add_entity(Entity {
            id: "alice".to_string(),
            entity_type: "person".to_string(),
            name: "Alice".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-seed".to_string(),
        })
        .await
        .unwrap();
    graph
        .add_entity(Entity {
            id: "bob".to_string(),
            entity_type: "person".to_string(),
            name: "Bob".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-seed".to_string(),
        })
        .await
        .unwrap();
    graph
        .add_entity(Entity {
            id: "ctxd".to_string(),
            entity_type: "project".to_string(),
            name: "ctxd".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-seed".to_string(),
        })
        .await
        .unwrap();
    graph
        .add_relationship(Relationship {
            id: "r1".to_string(),
            from_entity_id: "alice".to_string(),
            to_entity_id: "ctxd".to_string(),
            relationship_type: "authored".to_string(),
            properties: serde_json::json!({}),
            source_event_id: "evt-seed".to_string(),
        })
        .await
        .unwrap();

    // ctx_entities: type filter
    let out = server
        .ctx_entities(Parameters(EntitiesParams {
            entity_type: Some("person".to_string()),
            name_pattern: None,
            subject_pattern: None,
            limit: None,
            token: None,
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(arr.len(), 2);
    assert!(arr.iter().all(|e| e["entity_type"] == "person"));

    // ctx_entities: name substring
    let out = server
        .ctx_entities(Parameters(EntitiesParams {
            entity_type: None,
            name_pattern: Some("li".to_string()),
            subject_pattern: None,
            limit: None,
            token: None,
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(arr.len(), 1, "only Alice contains 'li'");
    assert_eq!(arr[0]["name"], "Alice");

    // ctx_entities: limit caps results
    let out = server
        .ctx_entities(Parameters(EntitiesParams {
            entity_type: None,
            name_pattern: None,
            subject_pattern: None,
            limit: Some(1),
            token: None,
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(arr.len(), 1);

    // ctx_related: alice → ctxd via "authored"
    let out = server
        .ctx_related(Parameters(RelatedParams {
            entity_id: "alice".to_string(),
            relationship_type: Some("authored".to_string()),
            token: None,
        }))
        .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["relationship"]["relationship_type"], "authored");
    assert_eq!(arr[0]["entity"]["id"], "ctxd");
}
