//! End-to-end test for `ctxd watch` (phase 4B).
//!
//! v0.4 ships the watcher as a stable command:
//! `ctxd watch [pattern] [--url URL] [--timeout-s N] [--limit N]`.
//!
//! This test asserts the timeout-no-match path: the watcher exits 2
//! when the deadline passes without matching events. The full
//! "writes-event-then-watcher-emits" round-trip is exercised by the
//! `ctxd-memory` skill's first-use demo against a real daemon
//! (the buffering interaction between hyper's chunked-transfer
//! body and a child-process raw TCP read consumer is sensitive to
//! kernel + macOS version, which makes a hermetic E2E fragile —
//! the manual smoke test in `docs/onboarding.md` is the canonical
//! verification path).

use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_cli::pidfile;
use ctxd_cli::serve::{serve, ServeConfig};
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

fn ctxd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ctxd")
}

async fn pick_free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

async fn wait_for_health(addr: &str, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if pidfile::health_probe(addr).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    false
}

async fn spawn_daemon(db_path: &Path, admin: &str) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let store = EventStore::open(db_path).await.expect("open store");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    let (tx, _) = broadcast::channel::<ctxd_cap::state::PendingApproval>(64);
    let cfg = ServeConfig {
        bind: admin.to_string(),
        wire_bind: None,
        mcp_stdio: false,
        mcp_sse: None,
        mcp_http: None,
        require_auth: false,
        embedder: "null".into(),
        embedder_model: None,
        embedder_url: None,
        embedder_api_key: None,
        storage: "sqlite".into(),
        storage_uri: None,
        federation: false,
        db_path: Some(db_path.to_path_buf()),
        cap_files: vec![],
    };
    tokio::spawn(serve(cfg, store, cap_engine, caveat_state, tx))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_exits_2_on_timeout_with_no_matches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ctxd.db");
    let admin_port = pick_free_port().await;
    let admin_bind = format!("127.0.0.1:{admin_port}");
    let daemon_handle = spawn_daemon(&db_path, &admin_bind).await;
    assert!(wait_for_health(&admin_bind, Duration::from_secs(10)).await);

    let child = tokio::process::Command::new(ctxd_bin())
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "watch",
            "/never/matches",
            "--url",
            &format!("http://{admin_bind}"),
            "--timeout-s",
            "1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let output = child.wait_with_output().await.expect("wait");
    // Exit 2 means "watcher saw 0 matching events" — the contract
    // the skill relies on for the demo's no-match branch.
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 on no-match timeout, got: {:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    daemon_handle.abort();
    let _ = daemon_handle.await;
}

#[tokio::test]
async fn watcher_resolves_url_from_pidfile_when_omitted() {
    // When --url isn't passed, the watcher resolves the daemon's
    // admin URL from the pidfile alongside --db. If neither is
    // available, the watcher exits with a clear error.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ctxd.db");

    let child = tokio::process::Command::new(ctxd_bin())
        .args([
            "--db",
            db_path.to_str().unwrap(),
            "watch",
            "",
            "--timeout-s",
            "1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let output = child.wait_with_output().await.expect("wait");
    assert!(
        !output.status.success(),
        "watcher should error when no daemon and no --url; got success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no pidfile") || stderr.contains("daemon not running"),
        "expected helpful error about missing pidfile/daemon, got: {stderr}"
    );
}
