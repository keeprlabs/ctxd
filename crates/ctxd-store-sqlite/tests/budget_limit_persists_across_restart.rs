//! Integration test: a `BudgetLimit` accumulated through one daemon
//! lifetime is still visible after the SQLite store closes and
//! re-opens. This is the core promise of the SQLite-backed
//! [`SqliteCaveatState`] — without persistence, a malicious caller
//! could just bounce the daemon to reset their budget.

use std::sync::Arc;
use std::time::Duration;

use ctxd_cap::state::{BudgetLimit, CaveatState, OperationCost};
use ctxd_cap::{CapEngine, Operation};
use ctxd_store_sqlite::caveat_state::SqliteCaveatState;
use ctxd_store_sqlite::EventStore;
use tempfile::TempDir;

#[tokio::test]
async fn budget_persists_across_open_close_open() {
    let tmp = TempDir::new().expect("tempdir");
    let db_path = tmp.path().join("ctxd.db");

    let engine = CapEngine::new();
    let budget = BudgetLimit {
        currency: "USD".to_string(),
        amount_micro_units: 5_000,
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
    let token_id = engine
        .extract_token_id(&token)
        .expect("token_id query")
        .expect("token has token_id");

    // First lifetime: charge 3 writes (3_000 µUSD).
    {
        let store = EventStore::open(&db_path).await.expect("open");
        let st: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store.clone()));
        for _ in 0..3 {
            engine
                .verify_with_state(
                    &token,
                    "/work/x",
                    Operation::Write,
                    None,
                    Some(st.as_ref()),
                    Duration::from_secs(1),
                )
                .await
                .expect("write");
        }
        assert_eq!(
            st.budget_get(&token_id, "USD").await.unwrap(),
            (3 * OperationCost::WRITE.0) as i64
        );
    }

    // Second lifetime: re-open the same database. Budget must still
    // be 3_000 — the cap is 5_000, so we should fit two more writes
    // and reject the sixth.
    let store = EventStore::open(&db_path).await.expect("re-open");
    let st: Arc<dyn CaveatState> = Arc::new(SqliteCaveatState::new(store));
    let prior = st.budget_get(&token_id, "USD").await.unwrap();
    assert_eq!(
        prior, 3_000,
        "budget didn't persist: expected 3_000 µUSD spent, got {prior}"
    );

    // Two more writes succeed (4_000, 5_000).
    for _ in 0..2 {
        engine
            .verify_with_state(
                &token,
                "/work/x",
                Operation::Write,
                None,
                Some(st.as_ref()),
                Duration::from_secs(1),
            )
            .await
            .expect("post-restart write");
    }

    // Sixth write must reject — exactly the cap then over.
    let res = engine
        .verify_with_state(
            &token,
            "/work/x",
            Operation::Write,
            None,
            Some(st.as_ref()),
            Duration::from_secs(1),
        )
        .await;
    assert!(
        res.is_err(),
        "post-restart spend should reject when over cap, got {res:?}"
    );
}
