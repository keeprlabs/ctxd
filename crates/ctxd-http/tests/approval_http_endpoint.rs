//! Integration test: `POST /v1/approvals/:id/decide` reaches the
//! `CaveatState` and updates the row, and `GET /v1/approvals` lists
//! pending rows. Exercises the in-process axum router.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ctxd_cap::state::{ApprovalDecision, CaveatState};
use ctxd_cap::CapEngine;
use ctxd_http::build_router;
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;
use http_body_util::BodyExt;
use tower::util::ServiceExt;

#[tokio::test]
async fn decide_endpoint_updates_caveat_state() {
    let store = EventStore::open_memory().await.expect("open memory store");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    // Seed a pending approval.
    caveat_state
        .approval_request("appr-test-1", "tok-1", "write", "/work/x")
        .await
        .expect("seed approval");

    let router = build_router(store.clone(), cap_engine, caveat_state.clone());

    // POST /v1/approvals/appr-test-1/decide  body={"decision":"allow"}
    let req = Request::builder()
        .method("POST")
        .uri("/v1/approvals/appr-test-1/decide")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"decision":"allow"}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK, "decide endpoint must 200");
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["approval_id"], "appr-test-1");
    assert_eq!(body["decision"], "allow");

    // The CaveatState must reflect the decision.
    let status = caveat_state
        .approval_status("appr-test-1")
        .await
        .expect("status");
    assert_eq!(status, ApprovalDecision::Allow);
}

#[tokio::test]
async fn decide_endpoint_rejects_unknown_decision() {
    let store = EventStore::open_memory().await.expect("open");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    caveat_state
        .approval_request("a", "t", "write", "/x")
        .await
        .unwrap();

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("POST")
        .uri("/v1/approvals/a/decide")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"decision":"maybe"}"#))
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn decide_endpoint_rejects_double_decide() {
    let store = EventStore::open_memory().await.expect("open");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    caveat_state
        .approval_request("a", "t", "write", "/x")
        .await
        .unwrap();

    let router = build_router(store, cap_engine, caveat_state);

    // First decide succeeds.
    let req1 = Request::builder()
        .method("POST")
        .uri("/v1/approvals/a/decide")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"decision":"allow"}"#))
        .unwrap();
    let resp1 = router.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second decide must reject (no Deny-flips-Allow).
    let req2 = Request::builder()
        .method("POST")
        .uri("/v1/approvals/a/decide")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"decision":"deny"}"#))
        .unwrap();
    let resp2 = router.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn list_approvals_endpoint_returns_pending() {
    let store = EventStore::open_memory().await.expect("open");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    caveat_state
        .approval_request("p1", "tok", "write", "/x")
        .await
        .unwrap();
    caveat_state
        .approval_request("p2", "tok", "search", "/y")
        .await
        .unwrap();
    // Decide p1 — it should drop out of the pending list.
    caveat_state
        .approval_decide("p1", ApprovalDecision::Allow)
        .await
        .unwrap();

    let router = build_router(store, cap_engine, caveat_state);
    let req = Request::builder()
        .method("GET")
        .uri("/v1/approvals")
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let pending = body["pending"].as_array().expect("pending array");
    assert_eq!(pending.len(), 1, "only p2 is pending after p1 decided");
    assert_eq!(pending[0]["approval_id"], "p2");
}
