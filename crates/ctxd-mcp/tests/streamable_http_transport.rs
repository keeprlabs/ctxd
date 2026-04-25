//! End-to-end test for the streamable-HTTP transport.
//!
//! Spawns a real listener on an ephemeral port, sends a `tools/call`
//! for `ctx_write`, then a `tools/call` for `ctx_read`, and asserts
//! the round-trip through the daemon process (no in-process shortcuts).

mod common;

use ctxd_mcp::auth::AuthPolicy;
use serde_json::Value;

#[tokio::test]
async fn streamable_http_write_then_read_round_trip() {
    let (server, _cap) = common::make_server().await;
    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Optional).await;

    // Write
    let body = common::tools_call_body(
        1,
        "ctx_write",
        serde_json::json!({
            "subject": "/http/echo",
            "event_type": "ctx.note",
            "data": r#"{"text":"hello-http"}"#,
        }),
    );
    let resp = common::http_post(addr, &body, None).await;
    assert_eq!(resp.status(), 200);
    let value = common::parse_http_response(resp).await;
    let result = result_object(&value);
    assert!(
        result.contains("/http/echo"),
        "expected write result to echo the subject: {result}"
    );
    assert!(
        !result.contains("error"),
        "write should not have errored: {result}"
    );

    // Read
    let body = common::tools_call_body(
        2,
        "ctx_read",
        serde_json::json!({
            "subject": "/http/echo",
            "recursive": false,
        }),
    );
    let resp = common::http_post(addr, &body, None).await;
    assert_eq!(resp.status(), 200);
    let value = common::parse_http_response(resp).await;
    let result = result_object(&value);
    assert!(
        result.contains("hello-http"),
        "expected the previously-written event back: {result}"
    );

    cancel.cancel();
}

/// Pull the human-readable text out of a JSON-RPC tool response. The
/// rmcp wrapper packs tool returns into `result.content[0].text`. We
/// don't bother with structured matching here — the tests only need
/// to assert the substring round-trips.
fn result_object(value: &Value) -> String {
    value
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| value.to_string())
}
