//! Crash-safety: events buffered but not yet flushed must survive a
//! hard restart. We simulate the crash by dropping the store without
//! calling `flush()`, then re-opening from the same root.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_duckobj::DuckObjStore;
use tempfile::TempDir;

#[tokio::test]
async fn wal_replay_restores_unflushed_events() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();

    // First session: append 100 events, drop without flushing.
    {
        let store = DuckObjStore::open_local(&root).await.unwrap();
        let subject = Subject::new("/wal/x").unwrap();
        for i in 0..100 {
            let e = Event::new(
                "ctxd://test".to_string(),
                subject.clone(),
                "demo".to_string(),
                serde_json::json!({"i": i}),
            );
            store.append(e).await.unwrap();
        }
        // Intentionally do NOT call store.flush(). Drop the handle —
        // the WAL is the only durability anchor for these events.
        drop(store);
    }

    // Second session: reopen and read.
    let store = DuckObjStore::open_local(&root).await.unwrap();
    let subject = Subject::new("/wal/x").unwrap();
    let events = store.read(&subject, false).await.unwrap();
    assert_eq!(events.len(), 100, "WAL should restore all unflushed events");
    // Ordering must be preserved.
    for (i, e) in events.iter().enumerate() {
        assert_eq!(
            e.data,
            serde_json::json!({"i": i}),
            "event {i} out of order or mangled"
        );
    }
}
