//! Integration test: `GET /v1/peers` returns every peer registered
//! in the store, sorted by `(added_at ASC, peer_id ASC)`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use ctxd_cap::state::CaveatState;
use ctxd_cap::{CapEngine, Operation};
use ctxd_http::build_router;
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::core::Peer;
use ctxd_store::EventStore;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

#[tokio::test]
async fn list_peers_returns_seeded_peers_in_order() {
    let store = EventStore::open_memory().await.expect("open memory");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    // Seed two peers. Peer "b" was added later, so it must appear last.
    let peer_a = Peer {
        peer_id: "peer-a".into(),
        url: "tcp://a.example:7778".into(),
        public_key: vec![0xaau8; 32],
        granted_subjects: vec!["/repo/a/*".into()],
        trust_level: serde_json::json!({"tier": "trusted"}),
        added_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    };
    let peer_b = Peer {
        peer_id: "peer-b".into(),
        url: "tcp://b.example:7778".into(),
        public_key: vec![0xbbu8; 32],
        granted_subjects: vec!["/repo/b/*".into(), "/notes/*".into()],
        trust_level: serde_json::json!({"tier": "probation"}),
        added_at: Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
    };
    store.peer_add_impl(peer_b.clone()).await.expect("seed b");
    store.peer_add_impl(peer_a.clone()).await.expect("seed a");

    // Mint an admin token so the request authorizes.
    let admin_token = cap_engine
        .mint("/**", &[Operation::Admin], None, None, None)
        .expect("mint admin");
    let admin_b64 = CapEngine::token_to_base64(&admin_token);

    let router = build_router(store.clone(), cap_engine, caveat_state);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/peers")
        .header("authorization", format!("Bearer {admin_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "list must 200");

    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let peers = body["peers"].as_array().expect("peers array");
    assert_eq!(peers.len(), 2);

    // Sort: (added_at ASC, peer_id ASC) → a before b.
    assert_eq!(peers[0]["peer_id"], "peer-a");
    assert_eq!(peers[1]["peer_id"], "peer-b");

    // Wire shape: `subject_patterns`, hex `public_key`, nullable `last_seen_at`.
    assert_eq!(peers[0]["url"], "tcp://a.example:7778");
    assert_eq!(peers[0]["public_key"], "aa".repeat(32));
    assert_eq!(peers[0]["subject_patterns"][0], "/repo/a/*");
    assert!(peers[0]["last_seen_at"].is_null());
    assert_eq!(peers[0]["added_at"], "2026-01-01T00:00:00+00:00");

    assert_eq!(peers[1]["public_key"], "bb".repeat(32));
    assert_eq!(peers[1]["subject_patterns"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn list_peers_empty_store_returns_empty_array() {
    let store = EventStore::open_memory().await.expect("open memory");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let admin_token = cap_engine
        .mint("/**", &[Operation::Admin], None, None, None)
        .expect("mint admin");
    let admin_b64 = CapEngine::token_to_base64(&admin_token);

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("GET")
        .uri("/v1/peers")
        .header("authorization", format!("Bearer {admin_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["peers"].as_array().unwrap().len(), 0);
}
