//! SQLite backend conformance tests.
//!
//! Re-runs the shared [`ctxd_store_core::testsuite`] against a fresh
//! in-memory SQLite store. If a backend change breaks trait-surface
//! behavior, these tests catch it.

use ctxd_store_core::testsuite;
use ctxd_store_sqlite::EventStore;

async fn fresh() -> EventStore {
    EventStore::open_memory()
        .await
        .expect("open in-memory SQLite store")
}

#[tokio::test]
async fn sqlite_runs_full_conformance_suite() {
    testsuite::run_all(fresh).await;
}
