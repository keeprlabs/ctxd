//! Real-TCP integration tests for the dashboard's serve composition.
//!
//! These exist because `tower::oneshot` doesn't populate
//! `ConnectInfo<SocketAddr>` and we need that for the loopback
//! middleware to function. Tests bind a real listener on
//! `127.0.0.1:0` and use `reqwest` as the client. They are
//! intentionally thin — handler-level behavior is covered in
//! `crates/ctxd-http/tests/dashboard_endpoints.rs`; what's verified
//! here is *the wiring*: the bind site uses
//! `into_make_service_with_connect_info`, the loopback middleware
//! lets loopback through, the host-check middleware rejects bad
//! `Host:` headers.

use std::sync::Arc;

use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_dashboard::apply_localhost_or_cap_token;
use ctxd_http::router::{build_router_with_hosts, default_allowed_hosts};
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;

/// Spin up a daemon-ish HTTP server in the same composition the
/// production `ctxd serve` uses, on an ephemeral port. Returns the
/// bound address and a JoinHandle that owns the server task.
async fn spawn_server() -> std::net::SocketAddr {
    let store = EventStore::open_memory().await.expect("open memory");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));

    let api = build_router_with_hosts(
        store,
        cap_engine.clone(),
        caveat_state,
        default_allowed_hosts(),
    );
    let frontend = ctxd_dashboard::router::<()>();
    let app = apply_localhost_or_cap_token(api.merge(frontend), cap_engine);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    // Tiny gate: give the server a moment to begin accepting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("reqwest client")
}

#[tokio::test]
async fn loopback_bypass_lets_dashboard_html_through() {
    let addr = spawn_server().await;
    let resp = client()
        .get(format!("http://{addr}/"))
        // Default allow-list is 127.0.0.1:7777 / localhost:7777 / [::1]:7777,
        // so we have to spoof the Host header to match — the bound port
        // is ephemeral.
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let body = resp.text().await.unwrap();
    assert!(body.starts_with("<!doctype html>"));
}

#[tokio::test]
async fn loopback_bypass_lets_static_assets_through() {
    let addr = spawn_server().await;
    let resp = client()
        .get(format!("http://{addr}/static/style.css"))
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/css"));
}

#[tokio::test]
async fn loopback_bypass_lets_v1_endpoints_through_without_token() {
    let addr = spawn_server().await;
    let resp = client()
        .get(format!("http://{addr}/v1/stats"))
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json["event_count"].is_u64());
    assert!(json["uptime_seconds"].is_u64());
}

#[tokio::test]
async fn host_header_evil_dot_com_rejected_with_421() {
    let addr = spawn_server().await;
    let resp = client()
        .get(format!("http://{addr}/v1/stats"))
        .header(reqwest::header::HOST, "evil.com")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::MISDIRECTED_REQUEST);
}

#[tokio::test]
async fn host_header_suffix_attack_rejected_with_421() {
    let addr = spawn_server().await;
    let resp = client()
        .get(format!("http://{addr}/v1/stats"))
        .header(reqwest::header::HOST, "127.0.0.1.evil.com")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::MISDIRECTED_REQUEST);
}

#[tokio::test]
async fn defensive_headers_present_on_every_response() {
    let addr = spawn_server().await;
    let resp = client()
        .get(format!("http://{addr}/v1/stats"))
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .expect("send");
    let h = resp.headers();
    assert!(h.contains_key("content-security-policy"));
    assert_eq!(h.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
}

#[tokio::test]
async fn hello_world_writes_one_event_via_loopback() {
    let addr = spawn_server().await;
    // Pre: 0 events.
    let stats: serde_json::Value = client()
        .get(format!("http://{addr}/v1/stats"))
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["event_count"], 0);

    // Post hello-world.
    let resp = client()
        .post(format!("http://{addr}/v1/dashboard/hello-world"))
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Post: 1 event.
    let stats: serde_json::Value = client()
        .get(format!("http://{addr}/v1/stats"))
        .header(reqwest::header::HOST, "127.0.0.1:7777")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["event_count"], 1);
}
