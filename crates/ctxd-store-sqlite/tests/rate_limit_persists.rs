//! Integration test: the rate-limit window survives a daemon restart.
//!
//! Without persistence a malicious caller could bounce the daemon to
//! reset their bucket and burst past the cap — the test pins both
//! halves of the contract (a) the in-window count is restored after
//! re-open, and (b) the next 1-second window admits a fresh hit. See
//! ADR 011.
//!
//! These cases also serve as the regression net for a future
//! token-bucket rewrite: any replacement must preserve "exactly N
//! verifies admitted within the same wall-clock second, period".

use std::sync::Arc;
use std::time::Duration;

use ctxd_cap::state::CaveatState;
use ctxd_store_sqlite::caveat_state::SqliteCaveatState;
use ctxd_store_sqlite::EventStore;
use tempfile::TempDir;

#[tokio::test]
async fn rate_window_count_persists_across_restart() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("ctxd.db");

    // Pick a small cap so we can exhaust it fast and have a clear
    // admit/deny boundary.
    let cap_per_sec: u32 = 3;

    // Lifetime 1 — burn 2 of 3 within the current window.
    {
        let store = EventStore::open(&db_path).await.expect("open db");
        let st: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store));

        // Wait for a window boundary so the two checks here, plus the
        // third one after restart, all share a wall-clock second.
        wait_for_window_start().await;

        assert!(st.rate_check("tok-1", cap_per_sec).await.unwrap());
        assert!(st.rate_check("tok-1", cap_per_sec).await.unwrap());
    }

    // Lifetime 2 — re-open the same DB. The third hit in the same
    // wall-clock second must succeed (count goes to 3 == cap), but
    // the fourth must reject.
    let store = EventStore::open(&db_path).await.expect("re-open db");
    let st: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store));

    assert!(
        st.rate_check("tok-1", cap_per_sec).await.unwrap(),
        "third hit at cap should be admitted"
    );
    assert!(
        !st.rate_check("tok-1", cap_per_sec).await.unwrap(),
        "fourth hit in same window must be rejected"
    );

    // Sleep past the window boundary; the next hit is admitted again.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        st.rate_check("tok-1", cap_per_sec).await.unwrap(),
        "post-rollover hit should be admitted"
    );
}

#[tokio::test]
async fn rate_check_independent_per_token() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("ctxd.db");
    let store = EventStore::open(&db_path).await.expect("open");
    let st: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store));

    wait_for_window_start().await;

    // Burn token A's 2-op cap.
    assert!(st.rate_check("tok-a", 2).await.unwrap());
    assert!(st.rate_check("tok-a", 2).await.unwrap());
    assert!(!st.rate_check("tok-a", 2).await.unwrap());

    // Token B's window is independent — first hit is admitted.
    assert!(
        st.rate_check("tok-b", 2).await.unwrap(),
        "second token shares no state with the first"
    );
}

/// Spin until we land within the first ~250 ms of the current second.
/// Without this, a test that does N hits could span two wall-clock
/// seconds and lose its admit/deny invariant.
async fn wait_for_window_start() {
    loop {
        let now = chrono::Utc::now();
        if now.timestamp_subsec_millis() < 250 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
