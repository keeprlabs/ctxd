//! Integration test: `BudgetLimit` is enforced and the 11th overspend
//! request is rejected with [`CapError::BudgetExceeded`].
//!
//! Mints a token with a 10_000 µUSD cap. Each `write` consumes
//! [`OperationCost::WRITE`] (1_000 µUSD). After ten successful writes
//! the budget should be exactly at the cap; the eleventh must reject.

use std::time::Duration;

use ctxd_cap::state::{BudgetLimit, CaveatState, InMemoryCaveatState, OperationCost};
use ctxd_cap::{CapEngine, CapError, Operation};

#[tokio::test]
async fn budget_limit_blocks_eleventh_write() {
    let engine = CapEngine::new();
    let budget = BudgetLimit {
        currency: "USD".to_string(),
        amount_micro_units: 10_000,
    };
    let token = engine
        .mint_full(
            "/**",
            &[Operation::Write],
            None,
            None,
            None,
            Some(&budget),
            &[],
        )
        .expect("mint");

    let state = InMemoryCaveatState::new();
    let timeout = Duration::from_secs(5);

    // Ten writes succeed.
    for i in 0..10 {
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Write,
                None,
                Some(&state),
                timeout,
            )
            .await
            .unwrap_or_else(|e| panic!("write {i} should succeed but got {e:?}"));
    }

    let token_id = engine
        .extract_token_id(&token)
        .expect("token_id query")
        .expect("token has token_id fact");
    assert_eq!(
        state.budget_get(&token_id, "USD").await.unwrap(),
        10_000,
        "after 10 writes, exactly at cap"
    );

    // Eleventh write must reject with BudgetExceeded.
    let res = engine
        .verify_with_state(
            &token,
            "/work/x",
            Operation::Write,
            None,
            Some(&state),
            timeout,
        )
        .await;
    match res {
        Err(CapError::BudgetExceeded {
            currency,
            spent,
            limit,
        }) => {
            assert_eq!(currency, "USD");
            assert_eq!(spent, 11_000);
            assert_eq!(limit, 10_000);
        }
        other => panic!("expected BudgetExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn budget_limit_zero_cost_op_does_not_charge() {
    // A `read` is `OperationCost::READ` = 0. With a tiny 1 µUSD cap, a
    // token should still allow infinite reads.
    let engine = CapEngine::new();
    let budget = BudgetLimit {
        currency: "USD".to_string(),
        amount_micro_units: 1,
    };
    let token = engine
        .mint_full(
            "/**",
            &[Operation::Read, Operation::Write],
            None,
            None,
            None,
            Some(&budget),
            &[],
        )
        .expect("mint");
    let state = InMemoryCaveatState::new();
    let timeout = Duration::from_secs(5);

    // Loop a handful of times — biscuit's per-call CPU budget can
    // trip under a tight-loop microbenchmark. The point of this test
    // is the *cost*, which is zero, not the throughput.
    for _ in 0..10 {
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
            .expect("read with zero cost");
    }
    assert_eq!(OperationCost::READ.as_i64(), 0);
}
