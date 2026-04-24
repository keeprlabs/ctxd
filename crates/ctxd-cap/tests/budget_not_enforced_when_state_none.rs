//! Integration test for the documented v0.2 fallback semantics: when
//! `state = None` is passed to [`CapEngine::verify_with_state`], the
//! `BudgetLimit` caveat is observed (the token still parses) but not
//! enforced — verify returns Ok and no counter advances. This is the
//! contract the rest of the codebase relies on so legacy call sites
//! that don't yet thread a `CaveatState` keep working.

use std::time::Duration;

use ctxd_cap::state::BudgetLimit;
use ctxd_cap::{CapEngine, Operation};

#[tokio::test]
async fn budget_with_none_state_returns_ok() {
    let engine = CapEngine::new();
    let budget = BudgetLimit {
        currency: "USD".to_string(),
        amount_micro_units: 1, // Tiny cap — would reject even one write.
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

    // Many writes with `state = None` all pass — caveat observed but
    // not enforced. Documented fallback per ADR 011. We loop a few
    // times rather than ~100 because biscuit's datalog evaluator has
    // its own per-call CPU budget and a hot loop can occasionally
    // trip it on a busy host.
    for _ in 0..10 {
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Write,
                None,
                None,
                Duration::from_secs(1),
            )
            .await
            .expect("write with no state should succeed");
    }
}

#[tokio::test]
async fn budget_facts_round_trip_through_extract() {
    // Sanity check: the budget caveat is *observable* even without
    // state — the token still emits the `budget_limit` fact and
    // `extract_stateful_caveats` can read it back.
    let engine = CapEngine::new();
    let budget = BudgetLimit {
        currency: "OPENAI_TOKENS".to_string(),
        amount_micro_units: 5_000_000,
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
    let stateful = engine.extract_stateful_caveats(&token).expect("extract");
    assert_eq!(
        stateful.budget_limit.as_ref().map(|b| &b.currency[..]),
        Some("OPENAI_TOKENS")
    );
    assert_eq!(
        stateful.budget_limit.as_ref().map(|b| b.amount_micro_units),
        Some(5_000_000)
    );
}
