//! End-to-end tests for the pidfile-aware daemon startup path landed
//! in phase 1A.
//!
//! Three scenarios:
//!
//! 1. `serve_writes_then_removes_pidfile_on_shutdown` — a normal
//!    serve cycle writes the pidfile after bind, the file matches
//!    this process's PID, and the file is removed when the serve
//!    task is aborted.
//! 2. `serve_refuses_to_start_when_live_daemon_holds_pidfile` — a
//!    fake "live daemon" (a stub HTTP listener answering /health
//!    with 200) plus a pidfile pointing at it makes a fresh
//!    `serve()` call return an error containing the friendly
//!    "ctxd is already running" wording.
//! 3. `serve_overwrites_stale_pidfile_with_dead_pid` — a pidfile
//!    pointing at a known-dead PID is overwritten on the next
//!    `serve()` call without complaint.
//!
//! These tests bind real ephemeral TCP ports rather than using
//! `tower::oneshot` because the pidfile is written from the bind
//! site itself; oneshot sidesteps the bind path entirely.

use ctxd_cap::state::CaveatState;
use ctxd_cap::CapEngine;
use ctxd_cli::pidfile::{self, PidFile};
use ctxd_cli::serve::{serve, ServeConfig};
use ctxd_store::caveat_state::SqliteCaveatState;
use ctxd_store::EventStore;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Pick an unused TCP port by binding on `:0` and returning the bound
/// port. The listener is dropped, so the kernel will likely re-assign
/// the same port to the next bind in this process.
async fn pick_free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Build a `ServeConfig` for a SQLite-backed daemon at the given DB
/// path and bind address, with all optional transports disabled.
fn cfg_for(db_path: &Path, bind: &str) -> ServeConfig {
    ServeConfig {
        bind: bind.to_string(),
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
    }
}

/// Open a fresh store + cap engine + caveat state at `db_path`.
async fn fresh_state(
    db_path: &Path,
) -> (
    EventStore,
    Arc<CapEngine>,
    Arc<dyn CaveatState>,
    broadcast::Sender<ctxd_cap::state::PendingApproval>,
) {
    let store = EventStore::open(db_path).await.expect("open store");
    let cap_engine = Arc::new(CapEngine::new());
    let caveat_state: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
    let (tx, _) = broadcast::channel::<ctxd_cap::state::PendingApproval>(64);
    (store, cap_engine, caveat_state, tx)
}

/// Wait up to `timeout` for `<addr>/health` to respond with 2xx via
/// the production `pidfile::health_probe` path. Returns `true` if it
/// does, `false` otherwise.
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

/// Same wait, but driven by `reqwest`. Used to cross-check whether a
/// failure is in the raw-HTTP probe vs the daemon itself.
async fn wait_for_health_reqwest(addr: &str, timeout: Duration) -> bool {
    let url = format!("http://{addr}/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .expect("reqwest client");
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if let Ok(r) = client
            .get(&url)
            .header(reqwest::header::HOST, addr)
            .send()
            .await
        {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    false
}

#[tokio::test]
async fn serve_writes_then_removes_pidfile_on_shutdown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ctxd.db");
    let port = pick_free_port().await;
    let bind = format!("127.0.0.1:{port}");

    let (store, cap_engine, caveat_state, tx) = fresh_state(&db_path).await;
    let cfg = cfg_for(&db_path, &bind);

    let serve_handle = tokio::spawn(serve(cfg, store, cap_engine, caveat_state, tx));

    // Wait for the daemon to be ready. /health must return 200 once
    // the listener is bound and the dashboard router is composed.
    assert!(
        wait_for_health(&bind, Duration::from_secs(5)).await,
        "daemon never came up at {bind}"
    );

    // Pidfile should be present and name THIS process.
    let pf = pidfile::read(&db_path).expect("pidfile must be written after bind");
    assert_eq!(pf.pid, std::process::id());
    assert_eq!(pf.admin_bind, bind);
    assert!(pf.version.starts_with("0."), "version field must be set");

    // Abort the serve task; the PidfileGuard's Drop should remove the
    // file. We give the runtime a brief moment to finalise the abort.
    serve_handle.abort();
    let _ = serve_handle.await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        pidfile::read(&db_path).is_none(),
        "pidfile must be removed when serve drops"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_refuses_to_start_when_live_daemon_holds_pidfile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ctxd.db");

    // Stand up a *real* ctxd daemon as the "live" one. Reusing the
    // production serve() is more faithful than a hand-rolled axum
    // app — it exercises the same host-check and loopback middleware
    // a real onboarding pre-flight will probe through.
    let live_port = pick_free_port().await;
    let live_bind = format!("127.0.0.1:{live_port}");
    let (store_live, cap_live, caveat_live, tx_live) = fresh_state(&db_path).await;
    let cfg_live = cfg_for(&db_path, &live_bind);
    let live_handle = tokio::spawn(serve(cfg_live, store_live, cap_live, caveat_live, tx_live));

    // Wait for the live daemon to be accepting. Both probe paths are
    // exercised so we can spot a regression in either independently.
    assert!(
        wait_for_health(&live_bind, Duration::from_secs(5)).await,
        "live daemon never answered raw /health probe at {live_bind}"
    );
    assert!(
        wait_for_health_reqwest(&live_bind, Duration::from_secs(2)).await,
        "live daemon never answered reqwest /health probe at {live_bind}"
    );

    // Sanity: detect should classify the live daemon as Running.
    let detected = pidfile::detect(&db_path).await;
    assert!(
        matches!(detected, pidfile::DaemonState::Running(_)),
        "expected detect() to return Running, got {detected:?}"
    );

    // Now try to start a *second* daemon against the same DB. The
    // pidfile pre-flight should refuse before bind.
    let new_port = pick_free_port().await;
    let bind = format!("127.0.0.1:{new_port}");
    let (store, cap_engine, caveat_state, tx) = fresh_state(&db_path).await;
    let cfg = cfg_for(&db_path, &bind);

    let result = serve(cfg, store, cap_engine, caveat_state, tx).await;
    let err = result.expect_err("expected serve to refuse start");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("ctxd is already running"),
        "expected 'ctxd is already running' in error, got: {msg}"
    );
    assert!(
        msg.contains(&live_bind),
        "error should surface the running daemon's admin URL ({live_bind}); got: {msg}"
    );

    live_handle.abort();
    let _ = live_handle.await;
}

#[tokio::test]
async fn serve_overwrites_stale_pidfile_with_dead_pid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("ctxd.db");

    // Write a pidfile that names a definitely-dead PID. Detect()
    // classifies this as Stale, and serve() should log + continue.
    pidfile::write(
        &db_path,
        &PidFile {
            pid: 999_999_999,
            admin_bind: "127.0.0.1:65535".into(),
            wire_bind: None,
            version: "0.0.0-stale".into(),
            started_at: chrono::Utc::now(),
            db_path: db_path.to_string_lossy().into(),
        },
    )
    .expect("write stale pidfile");

    let port = pick_free_port().await;
    let bind = format!("127.0.0.1:{port}");
    let (store, cap_engine, caveat_state, tx) = fresh_state(&db_path).await;
    let cfg = cfg_for(&db_path, &bind);
    let serve_handle = tokio::spawn(serve(cfg, store, cap_engine, caveat_state, tx));

    assert!(
        wait_for_health(&bind, Duration::from_secs(5)).await,
        "daemon never came up despite stale pidfile"
    );

    let pf = pidfile::read(&db_path).expect("pidfile must be present");
    assert_eq!(
        pf.pid,
        std::process::id(),
        "stale pidfile must have been overwritten with our PID"
    );
    assert_eq!(
        pf.admin_bind, bind,
        "stale admin_bind must have been overwritten"
    );
    assert_ne!(
        pf.version, "0.0.0-stale",
        "stale version must have been overwritten"
    );

    serve_handle.abort();
    let _ = serve_handle.await;
}
