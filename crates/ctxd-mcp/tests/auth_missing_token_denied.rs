//! `--require-auth` enforcement.
//!
//! When [`AuthPolicy::Required`] is in effect on an HTTP transport,
//! a `tools/call` with no token (neither header nor arg) must be
//! rejected at the middleware with HTTP 401. With the default
//! [`AuthPolicy::Optional`], the same call must pass through to the
//! tool surface (legacy stdio behaviour).
//!
//! We assert both directions on streamable-HTTP. Routes without a
//! tool call (e.g. `tools/list` or `initialize`) are allowed in
//! either policy — the require-auth gate fires only on `tools/call`.

mod common;

use ctxd_mcp::auth::AuthPolicy;

#[tokio::test]
async fn streamable_http_required_denies_missing_token() {
    let (server, _cap) = common::make_server().await;
    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Required).await;

    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({"subject": "/anything", "recursive": false}),
    );
    let resp = common::http_post(addr, &body, None).await;
    assert_eq!(resp.status(), 401, "expected 401 for unauthenticated call");

    cancel.cancel();
}

#[tokio::test]
async fn streamable_http_required_accepts_with_header() {
    use ctxd_cap::Operation;
    let (server, cap) = common::make_server().await;
    let token = common::mint(&cap, "/**", &[Operation::Read]);
    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Required).await;

    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({"subject": "/empty", "recursive": false}),
    );
    let auth = format!("Bearer {token}");
    let resp = common::http_post(addr, &body, Some(&auth)).await;
    assert_eq!(resp.status(), 200, "expected 200 with Authorization header");

    cancel.cancel();
}

#[tokio::test]
async fn streamable_http_optional_allows_missing_token() {
    let (server, _cap) = common::make_server().await;
    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Optional).await;

    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({"subject": "/anything", "recursive": false}),
    );
    let resp = common::http_post(addr, &body, None).await;
    assert_eq!(
        resp.status(),
        200,
        "default policy should be permissive (legacy stdio behaviour)"
    );

    cancel.cancel();
}

#[tokio::test]
async fn streamable_http_required_allows_arg_token() {
    use ctxd_cap::Operation;
    let (server, cap) = common::make_server().await;
    let token = common::mint(&cap, "/**", &[Operation::Read]);
    let (addr, cancel) = common::spawn_streamable_http(server, AuthPolicy::Required).await;

    // Caller provides the token via the per-call argument only.
    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({"subject": "/empty", "recursive": false, "token": token}),
    );
    let resp = common::http_post(addr, &body, None).await;
    assert_eq!(
        resp.status(),
        200,
        "an arg-only token should still satisfy require-auth"
    );

    cancel.cancel();
}
