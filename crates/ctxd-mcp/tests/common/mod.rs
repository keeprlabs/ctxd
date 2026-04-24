//! Shared helpers for the multi-transport integration tests.
//!
//! Each test in this folder spawns one or more transports against a
//! fresh in-memory store, exercises the public HTTP / SSE / stdio
//! surface, then triggers shutdown via the returned cancellation
//! token. We deliberately avoid stubbing rmcp internals — these tests
//! catch wiring bugs that unit-level mocks never see.

#![allow(dead_code)] // Some helpers are only used by a subset of test files.

use std::net::SocketAddr;
use std::sync::Arc;

use ctxd_cap::CapEngine;
use ctxd_mcp::auth::AuthPolicy;
use ctxd_mcp::CtxdMcpServer;
use ctxd_store::EventStore;
use tokio_util::sync::CancellationToken;

/// Spin up a fresh server with an in-memory store and a default cap
/// engine. The returned `CapEngine` is the same instance the server
/// uses, so tests can mint tokens that the server will then accept.
pub async fn make_server() -> (CtxdMcpServer, Arc<CapEngine>) {
    let store = EventStore::open_memory().await.expect("open store");
    let cap_engine = Arc::new(CapEngine::new());
    let server = CtxdMcpServer::new(store, cap_engine.clone(), "ctxd://test".to_string());
    (server, cap_engine)
}

/// Bind the streamable-HTTP transport on an ephemeral port. Returns
/// the chosen address and a token that, when cancelled, ends the
/// server. The transport is spawned on a tokio task — caller does not
/// need to await anything to keep it alive.
///
/// We bind a TCP listener up-front so the test can know the port
/// before the transport task wakes.
pub async fn spawn_streamable_http(
    server: CtxdMcpServer,
    policy: AuthPolicy,
) -> (SocketAddr, CancellationToken) {
    let cancel = CancellationToken::new();
    let cancel_for_server = cancel.clone();
    // Bind a listener to discover the port; close it immediately and
    // let `run_streamable_http` re-bind. Race window is microseconds
    // and tolerable for tests; the alternative would be to expose a
    // listener-accepting variant from the public API.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    tokio::spawn(async move {
        let _ =
            ctxd_mcp::transport::run_streamable_http(server, addr, policy, cancel_for_server).await;
    });

    // Give the transport a beat to actually bind. axum::serve binds
    // immediately on entry, but the rebind after our `drop(listener)`
    // can briefly race; a short retry loop on the client side is
    // simpler than a tighter coordination protocol.
    wait_for_bind(addr).await;
    (addr, cancel)
}

/// Same as [`spawn_streamable_http`] for the SSE transport.
pub async fn spawn_sse(
    server: CtxdMcpServer,
    policy: AuthPolicy,
) -> (SocketAddr, CancellationToken) {
    let cancel = CancellationToken::new();
    let cancel_for_server = cancel.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    tokio::spawn(async move {
        let _ = ctxd_mcp::transport::run_sse(server, addr, policy, cancel_for_server).await;
    });
    wait_for_bind(addr).await;
    (addr, cancel)
}

/// Poll-connect once for up to ~2s waiting for the server's listener
/// to be reachable. Test helper; production code uses graceful
/// startup signalling instead.
pub async fn wait_for_bind(addr: SocketAddr) {
    use tokio::time::{sleep, Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("server at {addr} never became reachable");
}

/// JSON-RPC body for `tools/list`.
pub const TOOLS_LIST_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;

/// Build a JSON-RPC body that calls `tools/call` for the given tool
/// name with the given arguments object.
pub fn tools_call_body(id: u64, tool: &str, arguments: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    }))
    .expect("serialize tools/call body")
}

/// JSON-RPC body that initialises a session — required before
/// `tools/list` / `tools/call` on any rmcp transport.
pub const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"ctxd-test","version":"1.0"}}}"#;

/// POST JSON-RPC over the streamable-HTTP transport at `/mcp` and
/// return the parsed response body. `auth_header` is added when
/// `Some(...)`. The streamable-HTTP transport in `json_response` mode
/// returns plain `application/json`, so we deserialize with `serde_json`.
pub async fn http_post(
    addr: SocketAddr,
    body: &str,
    auth_header: Option<&str>,
) -> reqwest::Response {
    let url = format!("http://{addr}/mcp");
    let mut req = reqwest::Client::new()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(body.to_string());
    if let Some(h) = auth_header {
        req = req.header("Authorization", h);
    }
    req.send().await.expect("send request")
}

/// Parse a streamable-HTTP JSON-RPC response. Works for both
/// `application/json` (json_response mode) and `text/event-stream`
/// where a single `data: <json>` line carries the response.
pub async fn parse_http_response(resp: reqwest::Response) -> serde_json::Value {
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.text().await.expect("read body");
    if ct.contains("application/json") {
        return serde_json::from_str(&body).expect("parse json");
    }
    // SSE: extract the first `data:` line and parse as JSON.
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) {
                return v;
            }
        }
    }
    panic!("no JSON-RPC response found in body: {body:?}")
}

/// Mint a base64-encoded biscuit token for a single subject + ops set.
pub fn mint(cap_engine: &CapEngine, subject: &str, ops: &[ctxd_cap::Operation]) -> String {
    let token = cap_engine
        .mint(subject, ops, None, None, None)
        .expect("mint token");
    CapEngine::token_to_base64(&token)
}
