//! `GET /v1/stats` returns the v0.4-extended shape: event_count,
//! subject_count, peer_count, pending_approval_count,
//! vector_embedding_count, uptime_seconds, version. All fields are
//! always present; older clients that only read `subject_count` keep
//! working.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_http::build_router;
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

#[tokio::test]
async fn stats_shape_empty_store() {
    let store = EventStore::open_memory().await.expect("open memory");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let router = build_router(store, cap_engine, caveat_state);
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/v1/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // Required fields, all populated as 0 / non-empty.
    assert_eq!(v["event_count"], 0);
    assert_eq!(v["subject_count"], 0);
    assert_eq!(v["peer_count"], 0);
    assert_eq!(v["pending_approval_count"], 0);
    assert_eq!(v["vector_embedding_count"], 0);
    assert!(v["uptime_seconds"].is_u64());
    assert!(v["version"].is_string());
    assert!(!v["version"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn stats_counts_grow_after_appends() {
    let store = EventStore::open_memory().await.expect("open memory");
    // Append 3 events across 2 subjects.
    for (subject, body) in [
        ("/work/notes/a", "alpha"),
        ("/work/notes/a", "alpha 2"),
        ("/me/preferences", "p"),
    ] {
        let ev = Event::new(
            "ctxd://test".into(),
            Subject::new(subject).unwrap(),
            "ctx.note".into(),
            serde_json::json!({"content": body}),
        );
        store.append(ev).await.expect("append");
    }

    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    let router = build_router(store, cap_engine, caveat_state);

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/v1/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["event_count"], 3);
    assert_eq!(v["subject_count"], 2);
}
