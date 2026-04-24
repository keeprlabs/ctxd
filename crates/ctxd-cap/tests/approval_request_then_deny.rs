//! Integration test: a token bearing `requires_approval(write)` blocks,
//! and when the approver decides Deny, `verify_with_state` returns
//! [`CapError::ApprovalDenied`].

use std::sync::Arc;
use std::time::Duration;

use ctxd_cap::state::{ApprovalDecision, CaveatState, InMemoryCaveatState};
use ctxd_cap::{CapEngine, CapError, Operation};

#[tokio::test]
async fn verify_blocks_then_deny_decides() {
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

    let approval_id = wait_for_pending(state.as_ref()).await;
    state
        .approval_decide(&approval_id, ApprovalDecision::Deny)
        .await
        .expect("decide");

    let result = verify_task.await.expect("join");
    match result {
        Err(CapError::ApprovalDenied { approval_id: id }) => {
            assert_eq!(id, approval_id);
        }
        other => panic!("expected ApprovalDenied, got {other:?}"),
    }
}

async fn wait_for_pending(state: &InMemoryCaveatState) -> String {
    for _ in 0..200 {
        if let Some(id) = state.test_first_pending_approval() {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no pending approval appeared within 2s");
}
