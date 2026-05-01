//! Integration tests for the v0.4 dashboard endpoints:
//! /v1/events list + by-id, /v1/subjects/tree, /v1/search,
//! /v1/dashboard/hello-world. The SSE stream endpoint
//! (/v1/events/stream) is exercised by a real-TCP test under
//! crates/ctxd-cli/tests/dashboard_serve.rs (Step 6) since
//! tower::oneshot doesn't drive SSE responses meaningfully.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_http::build_router;
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

async fn fixture_router() -> (axum::Router, EventStore) {
    let store = EventStore::open_memory().await.expect("open memory");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    let router = build_router(store.clone(), cap_engine, caveat_state);
    (router, store)
}

async fn append(store: &EventStore, subject: &str, body: &str) -> Event {
    let ev = Event::new(
        "ctxd://test".into(),
        Subject::new(subject).unwrap(),
        "ctx.note".into(),
        serde_json::json!({"content": body}),
    );
    store.append(ev).await.unwrap()
}

async fn json_get(router: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice::<serde_json::Value>(&body).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn list_events_empty_store_returns_empty_array() {
    let (router, _) = fixture_router().await;
    let (status, json) = json_get(&router, "/v1/events").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["events"].is_array());
    assert_eq!(json["events"].as_array().unwrap().len(), 0);
    assert!(json["next_cursor"].is_null());
}

#[tokio::test]
async fn list_events_newest_first_with_cursor_pagination() {
    let (router, store) = fixture_router().await;
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(append(&store, "/log", &format!("event {i}")).await.id);
    }

    // First page: limit=2 → events 4 then 3.
    let (s, page1) = json_get(&router, "/v1/events?limit=2").await;
    assert_eq!(s, StatusCode::OK);
    let evs = page1["events"].as_array().unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0]["id"], serde_json::Value::String(ids[4].to_string()));
    assert_eq!(evs[1]["id"], serde_json::Value::String(ids[3].to_string()));

    // Second page: opaque cursor.
    let cursor = page1["next_cursor"].as_str().unwrap();
    let (_, page2) =
        json_get(&router, &format!("/v1/events?limit=2&before={cursor}")).await;
    let evs = page2["events"].as_array().unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0]["id"], serde_json::Value::String(ids[2].to_string()));

    // Past end: empty.
    let cursor = page2["next_cursor"].as_str().unwrap();
    let (_, page3) =
        json_get(&router, &format!("/v1/events?limit=2&before={cursor}")).await;
    let evs = page3["events"].as_array().unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0]["id"], serde_json::Value::String(ids[0].to_string()));
}

#[tokio::test]
async fn list_events_invalid_cursor_400() {
    let (router, _) = fixture_router().await;
    let (s, _) = json_get(&router, "/v1/events?before=@@bogus@@").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn event_by_id_hit_and_404_and_400() {
    let (router, store) = fixture_router().await;
    let stored = append(&store, "/me", "x").await;

    let (s, ev) = json_get(&router, &format!("/v1/events/{}", stored.id)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ev["id"], serde_json::Value::String(stored.id.to_string()));

    let (s, _) = json_get(
        &router,
        "/v1/events/00000000-0000-0000-0000-000000000000",
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, _) = json_get(&router, "/v1/events/not-a-uuid").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn subject_tree_returns_cumulative_counts() {
    let (router, store) = fixture_router().await;
    append(&store, "/work/local/files/a.md", "a").await;
    append(&store, "/work/local/files/b.md", "b").await;
    append(&store, "/work/local/files/a.md", "a2").await;
    append(&store, "/me", "p").await;

    let (s, tree) = json_get(&router, "/v1/subjects/tree?prefix=/work").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tree["name"], "/work");
    // Cumulative count: 2 + 1 = 3 events under /work.
    assert_eq!(tree["count"], 3);
    assert!(tree["children"].is_array());
}

#[tokio::test]
async fn subject_tree_invalid_prefix_400() {
    let (router, _) = fixture_router().await;
    let (s, _) = json_get(&router, "/v1/subjects/tree?prefix=not-a-subject").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn search_hit_with_snippet() {
    let (router, store) = fixture_router().await;
    append(&store, "/notes/auth", "this is the authentication flow").await;
    append(&store, "/notes/billing", "monthly invoice").await;

    let (s, j) = json_get(&router, "/v1/search?q=authentication").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["query"], "authentication");
    let results = j["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert!(results[0]["snippet"].as_str().unwrap().contains("<mark>"));
    assert!(j["took_ms"].is_u64());
}

#[tokio::test]
async fn search_zero_matches_empty_results() {
    let (router, store) = fixture_router().await;
    append(&store, "/x", "alpha").await;
    let (s, j) = json_get(&router, "/v1/search?q=zzznonsensezzz").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(j["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn search_missing_q_400() {
    let (router, _) = fixture_router().await;
    let (s, _) = json_get(&router, "/v1/search").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _) = json_get(&router, "/v1/search?q=").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hello_world_writes_one_event() {
    let (router, store) = fixture_router().await;
    assert_eq!(store.event_count().await.unwrap(), 0);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/dashboard/hello-world")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(store.event_count().await.unwrap(), 1);
    let subjects = store.subjects(None, false).await.unwrap();
    assert_eq!(subjects, vec!["/dashboard/tutorial/hello".to_string()]);
}

#[tokio::test]
async fn hello_world_rejects_authenticated_callers() {
    // Even at the handler layer, hello-world refuses cap-token
    // callers — it's a loopback-only tutorial endpoint, not a remote
    // write surface.
    let (router, _) = fixture_router().await;
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/dashboard/hello-world")
                .header(header::AUTHORIZATION, "Bearer something")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
