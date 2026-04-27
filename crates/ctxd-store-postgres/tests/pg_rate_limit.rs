//! Postgres-backed `rate_check` parity tests. Mirrors the SQLite
//! `rate_limit_persists` integration suite so the two backends pin the
//! same windowed-counter contract: same-second hits accumulate, hits
//! past the cap reject, and a new wall-clock second resets the count.
//!
//! These tests are no-ops unless `CTXD_PG_URL` is set, matching the
//! rest of the postgres integration suite.

use std::sync::Arc;
use std::time::Duration;

use ctxd_cap::state::CaveatState;
use ctxd_store_postgres::caveat_state::PostgresCaveatState;
use ctxd_store_postgres::PostgresStore;
use sqlx::Executor;

fn pg_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var("CTXD_PG_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("[{test_name}] CTXD_PG_URL unset — skipping");
            None
        }
    }
}

async fn fresh_store() -> PostgresStore {
    let url = std::env::var("CTXD_PG_URL").expect("CTXD_PG_URL");
    let admin = sqlx::PgPool::connect(&url).await.expect("admin connect");
    let schema = format!("ctxd_test_{}", uuid::Uuid::now_v7().simple());
    admin
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create schema");
    drop(admin);
    let scoped = if url.contains('?') {
        format!("{url}&options=-c%20search_path%3D{schema}")
    } else {
        format!("{url}?options=-c%20search_path%3D{schema}")
    };
    PostgresStore::connect(&scoped).await.expect("open store")
}

async fn wait_for_window_start() {
    loop {
        let now = chrono::Utc::now();
        if now.timestamp_subsec_millis() < 250 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn pg_rate_check_admits_first_n_blocks_n_plus_one() {
    if pg_url_or_skip("pg_rate_check_admits_first_n_blocks_n_plus_one").is_none() {
        return;
    }
    let store = fresh_store().await;
    let st: Arc<dyn CaveatState> = Arc::new(PostgresCaveatState::new(store));

    wait_for_window_start().await;

    for _ in 0..5 {
        assert!(st.rate_check("tok-1", 5).await.unwrap());
    }
    assert!(
        !st.rate_check("tok-1", 5).await.unwrap(),
        "sixth hit in same second must reject"
    );
}

#[tokio::test]
async fn pg_rate_check_resets_on_next_window() {
    if pg_url_or_skip("pg_rate_check_resets_on_next_window").is_none() {
        return;
    }
    let store = fresh_store().await;
    let st: Arc<dyn CaveatState> = Arc::new(PostgresCaveatState::new(store));

    wait_for_window_start().await;

    for _ in 0..2 {
        assert!(st.rate_check("tok-1", 2).await.unwrap());
    }
    assert!(!st.rate_check("tok-1", 2).await.unwrap());

    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(
        st.rate_check("tok-1", 2).await.unwrap(),
        "post-rollover hit should be admitted"
    );
}

#[tokio::test]
async fn pg_rate_check_is_per_token() {
    if pg_url_or_skip("pg_rate_check_is_per_token").is_none() {
        return;
    }
    let store = fresh_store().await;
    let st: Arc<dyn CaveatState> = Arc::new(PostgresCaveatState::new(store));

    wait_for_window_start().await;

    assert!(st.rate_check("tok-a", 2).await.unwrap());
    assert!(st.rate_check("tok-a", 2).await.unwrap());
    assert!(!st.rate_check("tok-a", 2).await.unwrap());

    // Token B is independent.
    assert!(st.rate_check("tok-b", 2).await.unwrap());
}
