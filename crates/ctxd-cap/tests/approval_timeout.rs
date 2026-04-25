//! Integration test: an approval that nobody decides times out, and
//! `verify_with_state` returns [`CapError::ApprovalTimeout`].
//!
//! Uses real-time tokio (not paused/auto-advance) because the wait
//! path inside `InMemoryCaveatState::approval_wait` parks on a
//! `tokio::sync::Notify` AND a `tokio::time::sleep`. Pausing time
//! deadlocks `Notify` semantics. The real timeout we use is 100 ms,
//! so the test runs under a quarter second.

use std::time::{Duration, Instant};

use ctxd_cap::state::InMemoryCaveatState;
use ctxd_cap::{CapEngine, CapError, Operation};

#[tokio::test]
async fn verify_returns_approval_timeout() {
    let engine = CapEngine::new();
    let token = engine
        .mint_full(
            "/**",
            &[Operation::Write],
            None,
            None,
            None,
            None,
            &[Operation::Write],
        )
        .expect("mint");
    let state = InMemoryCaveatState::new();
    let timeout = Duration::from_millis(100);

    let started = Instant::now();
    let result = engine
        .verify_with_state(
            &token,
            "/work/x",
            Operation::Write,
            None,
            Some(&state),
            timeout,
        )
        .await;
    let elapsed = started.elapsed();

    // Hard-fail with the timeout variant.
    match result {
        Err(CapError::ApprovalTimeout { approval_id }) => {
            assert!(!approval_id.is_empty(), "approval id must be populated");
        }
        other => panic!("expected ApprovalTimeout, got {other:?}"),
    }
    // Sanity check: we actually waited around the timeout (with slack
    // for CI scheduling).
    assert!(
        elapsed >= timeout,
        "verify returned in {elapsed:?}, before timeout {timeout:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "verify took {elapsed:?}, far longer than expected"
    );
}
