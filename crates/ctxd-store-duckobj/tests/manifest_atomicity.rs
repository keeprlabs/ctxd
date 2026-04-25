//! Manifest atomicity: a torn manifest write (bytes appended past
//! the end of a valid JSON object) must not confuse readers. We
//! corrupt the manifest after a successful flush, reopen, and assert
//! the parsed manifest reflects only the committed parts.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_duckobj::DuckObjStore;
use std::io::Write;
use tempfile::TempDir;

#[tokio::test]
async fn reader_ignores_trailing_garbage_in_manifest() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();

    {
        let store = DuckObjStore::open_local(&root).await.unwrap();
        let subject = Subject::new("/atomic/x").unwrap();
        for i in 0..10 {
            let e = Event::new(
                "ctxd://test".to_string(),
                subject.clone(),
                "demo".to_string(),
                serde_json::json!({"i": i}),
            );
            store.append(e).await.unwrap();
        }
        store.flush().await.unwrap();
    }

    // Append garbage to the manifest — simulates a torn write on a
    // filesystem that doesn't honour PUT atomicity.
    let manifest_path = root.join("events").join("_manifest.json");
    assert!(manifest_path.exists(), "manifest should exist after flush");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&manifest_path)
        .unwrap();
    f.write_all(b"\x00\x00\x00{\"broken").unwrap();
    drop(f);

    // Reopen and confirm the 10 events are still readable.
    let store = DuckObjStore::open_local(&root).await.unwrap();
    let subject = Subject::new("/atomic/x").unwrap();
    let events = store.read(&subject, false).await.unwrap();
    assert_eq!(
        events.len(),
        10,
        "reader must tolerate torn manifest without losing committed parts"
    );
}
