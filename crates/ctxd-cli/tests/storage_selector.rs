//! End-to-end test for the `--storage` selector.
//!
//! For each backend kind we exercise the same minimal contract:
//!
//! 1. construct an `Arc<dyn Store>` via `select_store`,
//! 2. append a single event,
//! 3. read it back.
//!
//! The Postgres path is skipped when `CTXD_PG_URL` is unset (matches the
//! conformance suite). The DuckObj path uses a fresh tempdir.

use ctxd_cli::storage_selector::{select_store, StorageKind, StorageSpec};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use tempfile::TempDir;

/// Helper: round-trip an event through the supplied dyn-store handle.
async fn append_then_read_one(store: std::sync::Arc<dyn ctxd_store_core::Store>) {
    let subject = Subject::new("/storage_selector/x").unwrap();
    let e = Event::new(
        "ctxd://selector-test".into(),
        subject.clone(),
        "demo".into(),
        serde_json::json!({"v": 1}),
    );
    let stored = store.append(e).await.expect("append succeeds");
    let events = store.read(&subject, false).await.expect("read succeeds");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, stored.id);
}

#[tokio::test]
async fn sqlite_selector_constructs_a_working_store() {
    let td = TempDir::new().unwrap();
    let path = td.path().join("ctxd.db");
    let spec = StorageSpec {
        kind: StorageKind::Sqlite,
        sqlite_path: Some(path),
        uri: None,
    };
    let store = select_store(&spec).await.expect("sqlite selector");
    append_then_read_one(store).await;
}

#[cfg(feature = "storage-duckdb-object")]
#[tokio::test]
async fn duckdb_object_selector_constructs_a_working_store() {
    let td = TempDir::new().unwrap();
    let uri = format!("file://{}", td.path().display());
    let spec = StorageSpec {
        kind: StorageKind::DuckdbObject,
        sqlite_path: None,
        uri: Some(uri),
    };
    let store = select_store(&spec).await.expect("duckdb-object selector");
    append_then_read_one(store).await;
}

#[cfg(feature = "storage-postgres")]
#[tokio::test]
async fn postgres_selector_constructs_a_working_store() {
    let url = match std::env::var("CTXD_PG_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!(
                "[postgres_selector_constructs_a_working_store] CTXD_PG_URL unset — skipping"
            );
            return;
        }
    };
    let spec = StorageSpec {
        kind: StorageKind::Postgres,
        sqlite_path: None,
        uri: Some(url),
    };
    let store = select_store(&spec).await.expect("postgres selector");
    append_then_read_one(store).await;
}

#[tokio::test]
async fn unknown_kind_rejected_with_clear_error() {
    let err = StorageKind::parse("redis").unwrap_err();
    assert!(err.contains("unknown --storage"));
}
