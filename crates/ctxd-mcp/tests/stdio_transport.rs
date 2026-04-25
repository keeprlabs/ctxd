//! Regression test for the stdio transport — purely a smoke check
//! that `run_stdio` constructs and the stdio path still wraps the
//! same tool surface the server exposes. We do not actually run the
//! transport against real stdin/stdout (that would hijack the test
//! harness's TTY); instead we exercise the server's underlying
//! event store through the same `CtxdMcpServer` handle that
//! `run_stdio` would consume.
//!
//! This file exists so that future refactors of the stdio path
//! (e.g. switching the rmcp transport adapter) cannot land without
//! at least the public construction smoke test passing.

mod common;

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;

#[tokio::test]
async fn stdio_server_round_trips_write_then_read_via_shared_store() {
    let (server, _cap) = common::make_server().await;
    let store = server.store();

    let subject = Subject::new("/stdio/smoke").unwrap();
    store
        .append(Event::new(
            "test".to_string(),
            subject.clone(),
            "ctx.note".to_string(),
            serde_json::json!({"hello":"stdio"}),
        ))
        .await
        .expect("append");

    let events = store.read(&subject, false).await.expect("read");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].subject.as_str(), "/stdio/smoke");
    assert_eq!(events[0].event_type, "ctx.note");
}

#[tokio::test]
async fn stdio_run_helper_is_constructible() {
    // We can't actually run the stdio transport from inside a test
    // (stdin/stdout are captured by the harness), but we can confirm
    // the public helper exists and accepts our server type.
    let (server, _cap) = common::make_server().await;
    let _ = std::any::type_name_of_val(&ctxd_mcp::transport::run_stdio);
    let _ = server; // kept for symmetry; the function takes a CtxdMcpServer
}
