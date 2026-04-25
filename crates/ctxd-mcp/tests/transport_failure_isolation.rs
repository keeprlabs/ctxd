//! When one transport fails to bind, the others must keep working.
//!
//! We simulate an SSE bind failure by occupying the target port with
//! a sacrificial listener before launching SSE. The SSE task should
//! exit with a `TransportError::Startup`, but a separately-launched
//! streamable-HTTP transport sharing the same `CtxdMcpServer` must
//! remain reachable.
//!
//! We treat stdio as "always available" — there is no failure to
//! simulate from inside a unit test (stdio is the harness's own
//! TTY); the contract we're enforcing is "a sibling transport's
//! crash does not break the still-running ones."

mod common;

use ctxd_mcp::auth::AuthPolicy;
use ctxd_mcp::transport::TransportError;
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn sse_bind_failure_does_not_take_down_streamable_http() {
    let (server, _cap) = common::make_server().await;

    // Hold a listener on the port the SSE task will try to bind. The
    // bind will fail with EADDRINUSE.
    let blocker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let busy_addr: SocketAddr = blocker.local_addr().unwrap();

    let server_for_sse = server.clone();
    let cancel_sse = CancellationToken::new();
    let sse_handle = tokio::spawn({
        let cancel = cancel_sse.clone();
        async move {
            ctxd_mcp::transport::run_sse(server_for_sse, busy_addr, AuthPolicy::Optional, cancel)
                .await
        }
    });

    let sse_result = sse_handle.await.expect("sse task did not panic");
    assert!(
        matches!(sse_result, Err(TransportError::Startup(_))),
        "expected Startup error from busy SSE bind, got: {sse_result:?}"
    );

    // Streamable-HTTP transport on a fresh port — the previous bind
    // failure must not have torn down the shared server.
    let (http_addr, http_cancel) =
        common::spawn_streamable_http(server, AuthPolicy::Optional).await;
    let body = common::tools_call_body(
        1,
        "ctx_read",
        serde_json::json!({"subject": "/empty", "recursive": false}),
    );
    let resp = common::http_post(http_addr, &body, None).await;
    assert_eq!(
        resp.status(),
        200,
        "streamable-HTTP must keep working after SSE bind failure"
    );

    drop(blocker);
    http_cancel.cancel();
}
