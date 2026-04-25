//! Postgres backend conformance tests.
//!
//! Re-runs the shared [`ctxd_store_core::testsuite`] against a real
//! Postgres connection. The test is `#[ignore]` when `CTXD_PG_URL` is
//! unset so CI without a Postgres service container skips cleanly.
//!
//! ## Running locally
//!
//! ```bash
//! docker run --rm -d --name pg -p 5432:5432 -e POSTGRES_PASSWORD=test postgres:16
//! CTXD_PG_URL=postgres://postgres:test@localhost:5432/postgres \
//!   cargo test -p ctxd-store-postgres --test conformance -- --include-ignored
//! ```
//!
//! Each test gets its own freshly-named schema so suites don't leak
//! state between runs and parallel test execution stays clean.

use ctxd_store_core::testsuite;
use ctxd_store_postgres::PostgresStore;
use sqlx::Executor;

/// Spin up a fresh schema and return a `PostgresStore` backed by it.
///
/// We wedge `search_path` so all migrations land in the test-private
/// schema; dropping the schema at the end of the test is the
/// responsibility of the runner, not us — leftover schemas in CI's
/// ephemeral container cost nothing, and the next test run won't
/// collide because every schema name embeds a UUIDv7.
async fn fresh_store() -> PostgresStore {
    let url = std::env::var("CTXD_PG_URL")
        .expect("CTXD_PG_URL must be set for postgres conformance tests");
    let admin_pool = sqlx::PgPool::connect(&url)
        .await
        .expect("connect to postgres");
    let schema = format!("ctxd_test_{}", uuid::Uuid::now_v7().simple());
    admin_pool
        .execute(format!("CREATE SCHEMA \"{schema}\"").as_str())
        .await
        .expect("create test schema");
    drop(admin_pool);

    // Re-connect with `options=-c search_path=...` so every
    // migration runs inside our schema.
    let scoped_url = if url.contains('?') {
        format!("{url}&options=-c%20search_path%3D{schema}")
    } else {
        format!("{url}?options=-c%20search_path%3D{schema}")
    };
    PostgresStore::connect(&scoped_url)
        .await
        .expect("open postgres store")
}

/// Skip when `CTXD_PG_URL` is not set. Runtime skip rather than
/// `#[ignore]` because cargo's `--include-ignored` would otherwise be
/// required, which makes local dev harder. CI sets the variable
/// unconditionally.
fn pg_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var("CTXD_PG_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("[{test_name}] CTXD_PG_URL unset — skipping");
            None
        }
    }
}

#[tokio::test]
async fn postgres_runs_full_conformance_suite() {
    if pg_url_or_skip("postgres_runs_full_conformance_suite").is_none() {
        return;
    }
    testsuite::run_all(fresh_store).await;
}
