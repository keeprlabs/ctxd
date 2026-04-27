//! Integration test: auth gating on `/v1/peers` and
//! `/v1/peers/:peer_id`.
//!
//! - No `Authorization` header → 401.
//! - Bearer token without `Admin` scope → 403.
//! - Bearer token with `Admin` scope → 200 / 204 as appropriate.

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
use tower::util::ServiceExt;

fn sample_peer(peer_id: &str) -> Peer {
    Peer {
        peer_id: peer_id.into(),
        url: format!("tcp://{peer_id}.example:7778"),
        public_key: vec![0x42u8; 32],
        granted_subjects: vec!["/x/*".into()],
        trust_level: serde_json::json!({}),
        added_at: Utc::now(),
    }
}

#[tokio::test]
async fn list_peers_without_token_returns_401() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("GET")
        .uri("/v1/peers")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_peers_with_non_admin_token_returns_403() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    // Read-only token: not Admin.
    let read_token = cap_engine
        .mint("/**", &[Operation::Read], None, None, None)
        .unwrap();
    let read_b64 = CapEngine::token_to_base64(&read_token);

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("GET")
        .uri("/v1/peers")
        .header("authorization", format!("Bearer {read_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_peers_with_admin_token_returns_200() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let admin_token = cap_engine
        .mint("/**", &[Operation::Admin], None, None, None)
        .unwrap();
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
}

#[tokio::test]
async fn delete_peer_without_token_returns_401() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    store.peer_add_impl(sample_peer("p1")).await.unwrap();
    let router = build_router(store, cap_engine, caveat_state);

    let req = Request::builder()
        .method("DELETE")
        .uri("/v1/peers/p1")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_peer_with_non_admin_token_returns_403() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    store.peer_add_impl(sample_peer("p1")).await.unwrap();
    let write_token = cap_engine
        .mint("/**", &[Operation::Write], None, None, None)
        .unwrap();
    let write_b64 = CapEngine::token_to_base64(&write_token);

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("DELETE")
        .uri("/v1/peers/p1")
        .header("authorization", format!("Bearer {write_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_peer_with_admin_token_returns_204() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    store.peer_add_impl(sample_peer("p1")).await.unwrap();
    let admin_token = cap_engine
        .mint("/**", &[Operation::Admin], None, None, None)
        .unwrap();
    let admin_b64 = CapEngine::token_to_base64(&admin_token);

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("DELETE")
        .uri("/v1/peers/p1")
        .header("authorization", format!("Bearer {admin_b64}"))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn malformed_bearer_returns_401() {
    let store = EventStore::open_memory().await.unwrap();
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let router = build_router(store, cap_engine, caveat_state);
    // No "Bearer " prefix → reads as missing.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/peers")
        .header("authorization", "not-a-bearer-token")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
