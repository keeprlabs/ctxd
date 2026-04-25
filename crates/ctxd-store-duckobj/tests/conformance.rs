//! DuckObj backend conformance — runs the shared
//! [`ctxd_store_core::testsuite`] against a DuckObj store backed by a
//! local-filesystem object store in a tempdir.
//!
//! The fresh-store factory creates a brand-new tempdir per invocation
//! so tests never share state.

use std::sync::Arc;

use ctxd_store_core::testsuite;
use ctxd_store_duckobj::DuckObjStore;
use tempfile::TempDir;

/// Build a brand-new store rooted in a freshly-allocated tempdir.
///
/// The tempdir must outlive the store, so we intentionally forget
/// it — the process-local OS tempdir cleanup (CI sandbox, `TMPDIR`
/// sweeping, `/tmp`) reclaims the space between runs. We do this
/// because `testsuite::with_store` takes ownership of the store by
/// value and the factory signature does not surface any way to
/// return an auxiliary guard.
async fn fresh() -> DuckObjStore {
    let td = TempDir::new().expect("tempdir");
    let root = td.path().to_path_buf();
    std::mem::forget(td);
    DuckObjStore::open_local(&root).await.expect("open duckobj")
}

#[tokio::test]
async fn duckobj_runs_full_conformance_suite() {
    testsuite::run_all(fresh).await;
}

#[tokio::test]
async fn duckobj_store_is_dyn_store_compatible() {
    // Sanity check: the store can be held behind `Arc<dyn Store>`,
    // which is what the CLI selector returns.
    let s = fresh().await;
    let dyn_store: Arc<dyn ctxd_store_core::Store> = Arc::new(s);
    let subjects = dyn_store.subjects(None, false).await.unwrap();
    assert!(subjects.is_empty());
}
