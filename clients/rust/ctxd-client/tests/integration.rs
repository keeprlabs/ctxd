//! End-to-end integration tests against a real `ctxd serve` process.
//!
//! These tests spawn the binary built from this workspace, exercise
//! the SDK's high-level [`CtxdClient`] surface, and shut down cleanly.
//! They are deliberately the slow tier of the test suite — running
//! against a real daemon catches surface mismatches the unit tests
//! can't see (clap parsing, route layering, schema drift).

use std::time::Duration;

use ctxd_client::{CtxdClient, CtxdError, Operation, QueryView};

mod common;

/// Shared boot path: spawn daemon + return a connected SDK client.
async fn boot() -> (common::DaemonHandle, CtxdClient) {
    let daemon = common::spawn_daemon().await;
    let client = CtxdClient::connect(&daemon.http_url)
        .await
        .expect("connect http");
    let client = client
        .with_wire(&daemon.wire_addr)
        .await
        .expect("connect wire");
    (daemon, client)
}

#[tokio::test]
async fn health_returns_v0_3_x_version() {
    let (_d, client) = boot().await;
    let info = client.health().await.expect("health");
    assert_eq!(info.status, "ok");
    assert!(
        info.version.starts_with("0.3."),
        "expected version starting 0.3.x, got {}",
        info.version
    );
}

#[tokio::test]
async fn write_then_query_log_returns_event() {
    let (_d, client) = boot().await;
    let id = client
        .write(
            "/sdk-test/log/one",
            "ctx.note",
            serde_json::json!({"content": "hello sdk"}),
        )
        .await
        .expect("write");

    // Query the log view and confirm the event is present.
    let events = client
        .query("/sdk-test/log/one", QueryView::Log)
        .await
        .expect("query");
    assert!(
        events.iter().any(|e| e.id == id),
        "queried log did not contain the just-written event id {id}"
    );
}

#[tokio::test]
async fn subscribe_yields_published_event() {
    let (_d, client) = boot().await;
    let mut stream = client
        .subscribe("/sdk-test/sub/**")
        .await
        .expect("subscribe");

    // Spawn the publish on a small delay so the subscription is
    // definitely registered first. The daemon's `Sub` handler
    // attaches a broadcast receiver before sending the first frame
    // back, but we're racing with the TCP buffer flush — the
    // 50ms delay sidesteps the race entirely on every CI we've
    // seen.
    let client_pub = client.clone();
    let pub_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        client_pub
            .write(
                "/sdk-test/sub/event",
                "ctx.note",
                serde_json::json!({"msg": "via sub"}),
            )
            .await
    });

    // Read with a hard cap. If the daemon never delivers, we'd hang
    // the test forever otherwise.
    let event = tokio::time::timeout(Duration::from_secs(5), stream.next_event())
        .await
        .expect("subscription timeout")
        .expect("subscription error")
        .expect("subscription end-of-stream before event");

    let written_id = pub_task.await.expect("join").expect("write");
    assert_eq!(event.id, written_id, "subscribed event id != written id");
    assert_eq!(event.event_type, "ctx.note");
}

#[tokio::test]
async fn grant_returns_base64_token() {
    let (_d, client) = boot().await;
    let token = client
        .grant(
            "/sdk-test/grant/**",
            &[Operation::Read, Operation::Subjects],
            None,
        )
        .await
        .expect("grant");
    assert!(!token.is_empty(), "token must be non-empty");
    // Biscuit base64 tokens use the URL-safe alphabet; assert it's
    // at least plausible base64 (only [A-Za-z0-9_\-=]) and long
    // enough to be a real token.
    assert!(token.len() > 32, "token suspiciously short: {token}");
    for c in token.chars() {
        assert!(
            c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '=',
            "non-base64 char in token: {c:?}"
        );
    }
}

#[tokio::test]
async fn revoke_via_wire_protocol_returns_not_implemented() {
    let (_d, client) = boot().await;
    // Today the daemon's REVOKE handler returns
    //   Response::Error { message: "REVOKE is not implemented, scheduled for v0.2" }
    // The SDK surfaces that as `UnexpectedWireResponse` so callers
    // can detect-and-handle. When the daemon implements REVOKE this
    // test should switch to asserting Ok — that's the exact intent.
    let err = client
        .revoke("any-id")
        .await
        .expect_err("daemon REVOKE is a stub today");
    let msg = format!("{err}");
    assert!(
        msg.contains("REVOKE") || msg.contains("not implemented"),
        "expected stub-error message, got: {msg}"
    );
}

#[tokio::test]
async fn peers_starts_empty_and_remove_404s_on_unknown() {
    let (_d, client) = boot().await;

    // /v1/peers requires admin; mint one and re-attach.
    let admin_token = client
        .grant("/", &[Operation::Admin], None)
        .await
        .expect("mint admin");
    let admin_client = CtxdClient::connect(&_d.http_url)
        .await
        .expect("connect")
        .with_token(admin_token);

    let peers = admin_client.peers().await.expect("peers");
    assert!(
        peers.is_empty(),
        "fresh daemon must have zero peers, got {peers:?}"
    );

    let err = admin_client
        .peer_remove("does-not-exist")
        .await
        .expect_err("remove unknown peer must fail");
    match err {
        CtxdError::NotFound(_) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn stats_returns_subject_count() {
    let (_d, client) = boot().await;
    // Pre-write so subject_count is non-zero.
    client
        .write(
            "/sdk-test/stats/one",
            "ctx.note",
            serde_json::json!({"k": "v"}),
        )
        .await
        .expect("write");
    let stats = client.stats().await.expect("stats");
    assert!(
        stats.subject_count >= 1,
        "subject_count must be >= 1 after a write, got {}",
        stats.subject_count
    );
}

#[tokio::test]
async fn write_without_wire_returns_clear_error() {
    // Don't use the harness — only configure HTTP.
    let _d = common::spawn_daemon().await;
    let client = CtxdClient::connect(&_d.http_url).await.expect("connect");
    let err = client
        .write("/x", "demo", serde_json::json!({}))
        .await
        .expect_err("must fail");
    assert!(matches!(err, CtxdError::WireNotConfigured));
}
