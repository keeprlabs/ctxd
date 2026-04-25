//! Integration test: a token bearing `requires_approval(write)` blocks
//! `verify_with_state` until another task calls `approval_decide(allow)`,
//! at which point verify resolves Ok.

use std::sync::Arc;
use std::time::Duration;

use ctxd_cap::state::{ApprovalDecision, CaveatState, InMemoryCaveatState};
use ctxd_cap::{CapEngine, Operation};

#[tokio::test]
async fn verify_blocks_then_allow_decides() {
    let engine = Arc::new(CapEngine::new());
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

    let state = Arc::new(InMemoryCaveatState::new());
    let timeout = Duration::from_secs(5);

    // Spawn the verifier — it must block.
    let engine_b = engine.clone();
    let state_b: Arc<dyn CaveatState> = state.clone();
    let token_b = token.clone();
    let verify_task = tokio::spawn(async move {
        engine_b
            .verify_with_state(
                &token_b,
                "/work/x",
                Operation::Write,
                None,
                Some(state_b.as_ref()),
                timeout,
            )
            .await
    });

    // Wait for the verifier to register an approval row.
    let approval_id = wait_for_pending_approval(state.as_ref()).await;

    state
        .approval_decide(&approval_id, ApprovalDecision::Allow)
        .await
        .expect("decide");

    verify_task.await.expect("join").expect("verify");
}

/// Poll until at least one pending approval shows up. Used by tests
/// that drive `verify_with_state` in a spawned task and then need to
/// pluck the approval id back out.
async fn wait_for_pending_approval(state: &InMemoryCaveatState) -> String {
    // We cheat: the in-memory impl exposes nothing to enumerate
    // approvals, so we poll a fixed list of recently-generated UUIDs?
    // Better approach: derive id by scanning the approval map via a
    // helper. Instead, expose via debug printout? — keep it simple by
    // adding a small enumerate helper just for tests via downcast.
    //
    // Pragmatic solution: spin until the approval map has exactly one
    // entry and read its id via the trait helper we add below.
    for _ in 0..200 {
        if let Some(id) = state.first_pending_approval() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no pending approval appeared within 2s");
}

/// Helper trait we attach to `InMemoryCaveatState` for tests only.
trait FirstPending {
    fn first_pending_approval(&self) -> Option<String>;
}

impl FirstPending for InMemoryCaveatState {
    fn first_pending_approval(&self) -> Option<String> {
        // Test-only: read internals via the publicly visible test
        // helper added in `state.rs`.
        self.test_first_pending_approval()
    }
}
