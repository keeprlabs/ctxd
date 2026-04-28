//! Integration test: `rate_limit_ops_per_sec` is enforced.
//!
//! Mints a token with a 5 ops/sec cap. The first five verifies in the
//! same second succeed; the sixth must reject with
//! [`CapError::RateLimited`]. Sleeping past the next 1-second window
//! boundary admits a fresh hit.
//!
//! These cases pin the *exact* admission semantics of the windowed
//! counter so a future "smoother" token-bucket replacement (see ADR
//! 011) has a regression net to start from.

use std::time::Duration;

use ctxd_cap::state::InMemoryCaveatState;
use ctxd_cap::{CapEngine, CapError, Operation};

/// Spin until we land within the first ~250 ms of a wall-clock second
/// boundary. The rate-limit window is keyed on `now().timestamp()`, so
/// without this the test could spend its entire 5-hit budget across
/// two adjacent windows by accident — a flake we'd rather not eat.
async fn wait_for_window_start() {
    loop {
        let now = chrono::Utc::now();
        // `subsec_millis()` is the ms past the second floor.
        if now.timestamp_subsec_millis() < 250 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn rate_limit_admits_first_n_blocks_n_plus_one() {
    let engine = CapEngine::new();
    // Cap: 5 ops/sec.
    let token = engine
        .mint("/**", &[Operation::Read], None, None, Some(5))
        .expect("mint with rate_limit_ops_per_sec=5");

    let state = InMemoryCaveatState::new();
    let timeout = Duration::from_secs(5);

    wait_for_window_start().await;

    // Hits 1..=5 within the same second all succeed.
    for i in 1..=5 {
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Read,
                None,
                Some(&state),
                timeout,
            )
            .await
            .unwrap_or_else(|e| panic!("hit {i} should succeed but got {e:?}"));
    }

    // Hit 6 in the same window must reject.
    let res = engine
        .verify_with_state(
            &token,
            "/work/x",
            Operation::Read,
            None,
            Some(&state),
            timeout,
        )
        .await;
    match res {
        Err(CapError::RateLimited { ops_per_sec }) => {
            assert_eq!(
                ops_per_sec, 5,
                "RateLimited should surface the declared cap"
            );
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn rate_limit_resets_on_next_window() {
    let engine = CapEngine::new();
    let token = engine
        .mint("/**", &[Operation::Read], None, None, Some(2))
        .expect("mint");

    let state = InMemoryCaveatState::new();
    let timeout = Duration::from_secs(5);

    wait_for_window_start().await;

    // Burn through the cap in the current window.
    for _ in 0..2 {
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Read,
                None,
                Some(&state),
                timeout,
            )
            .await
            .unwrap();
    }
    assert!(matches!(
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Read,
                None,
                Some(&state),
                timeout,
            )
            .await,
        Err(CapError::RateLimited { ops_per_sec: 2 })
    ));

    // Sleep past the next second boundary — the window should reset.
    // 1.2s is enough to clear any second we started in plus the ~250ms
    // we waited for at the start.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Fresh window admits another hit.
    engine
        .verify_with_state(
            &token,
            "/work/x",
            Operation::Read,
            None,
            Some(&state),
            timeout,
        )
        .await
        .expect("post-rollover hit should succeed");
}

#[tokio::test]
async fn rate_limit_absent_means_unlimited() {
    // No `rate_limit_ops_per_sec` fact on the token = no enforcement.
    // This protects v0.2 tokens that never carried the fact.
    let engine = CapEngine::new();
    let token = engine
        .mint("/**", &[Operation::Read], None, None, None)
        .expect("mint without rate cap");
    let state = InMemoryCaveatState::new();
    let timeout = Duration::from_secs(5);

    for _ in 0..50 {
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Read,
                None,
                Some(&state),
                timeout,
            )
            .await
            .expect("should never rate-limit when fact is absent");
    }
}
