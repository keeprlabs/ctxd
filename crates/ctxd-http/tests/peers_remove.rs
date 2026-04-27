//! Integration test: `DELETE /v1/peers/:peer_id` removes the row
//! (204 + GET no longer lists it) and returns 404 for unknown ids.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use ctxd_cap::state::CaveatState;
use ctxd_cap::{CapEngine, Operation};
use ctxd_http::build_router;
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::core::Peer;
use ctxd_store::EventStore;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

fn sample_peer(peer_id: &str, byte: u8) -> Peer {
    Peer {
        peer_id: peer_id.into(),
        url: format!("tcp://{peer_id}.example:7778"),
        public_key: vec![byte; 32],
        granted_subjects: vec!["/x/*".into()],
        trust_level: serde_json::json!({}),
        added_at: Utc::now(),
    }
}

#[tokio::test]
async fn delete_existing_peer_returns_204_and_disappears_from_list() {
    let store = EventStore::open_memory().await.expect("open");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    store.peer_add_impl(sample_peer("p1", 0x01)).await.unwrap();
    store.peer_add_impl(sample_peer("p2", 0x02)).await.unwrap();

    let admin_token = cap_engine
        .mint("/**", &[Operation::Admin], None, None, None)
        .unwrap();
    let admin_b64 = CapEngine::token_to_base64(&admin_token);

    let router = build_router(store.clone(), cap_engine, caveat_state);

    // DELETE /v1/peers/p1 → 204
    let req = Request::builder()
        .method("DELETE")
        .uri("/v1/peers/p1")
        .header("authorization", format!("Bearer {admin_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "delete must 204");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(body.is_empty(), "204 must have empty body");

    // GET /v1/peers — only p2 remains.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/peers")
        .header("authorization", format!("Bearer {admin_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let peers = body["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["peer_id"], "p2");
}

#[tokio::test]
async fn delete_unknown_peer_returns_404() {
    let store = EventStore::open_memory().await.expect("open");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let admin_token = cap_engine
        .mint("/**", &[Operation::Admin], None, None, None)
        .unwrap();
    let admin_b64 = CapEngine::token_to_base64(&admin_token);

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("DELETE")
        .uri("/v1/peers/does-not-exist")
        .header("authorization", format!("Bearer {admin_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "missing peer must 404"
    );
}
