//! Concurrent appenders: 8 tasks each append 500 events. Assert
//! no seq duplicates, no part-name collisions, and the full 8*500
//! event set is readable at the end.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_duckobj::DuckObjStore;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn concurrent_appenders_8_tasks_500_events_each() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();
    let store = Arc::new(DuckObjStore::open_local(&root).await.unwrap());

    let mut handles = Vec::new();
    for task in 0..8 {
        let s = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for i in 0..500 {
                let subject = Subject::new(&format!("/concurrent/task{task}")).unwrap();
                let e = Event::new(
                    "ctxd://test".to_string(),
                    subject,
                    "demo".to_string(),
                    serde_json::json!({"task": task, "i": i}),
                );
                s.append(e).await.unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    store.flush().await.unwrap();

    // All 4000 events readable.
    let root_sub = Subject::new("/concurrent").unwrap();
    let events = store.read(&root_sub, true).await.unwrap();
    assert_eq!(events.len(), 4000);

    // No duplicate event ids.
    let mut ids = std::collections::HashSet::new();
    for e in &events {
        assert!(ids.insert(e.id), "duplicate event id: {}", e.id);
    }

    // Per-task, exactly 500 events.
    for task in 0..8 {
        let s = Subject::new(&format!("/concurrent/task{task}")).unwrap();
        let ev = store.read(&s, false).await.unwrap();
        assert_eq!(ev.len(), 500, "task {task} had {} events", ev.len());
    }

    // Part names on disk must be unique — verify by listing.
    let mut part_names = std::collections::HashSet::new();
    walk_and_collect_parquet(&root.join("events"), &mut part_names);
    assert!(
        !part_names.is_empty(),
        "expected at least one Parquet part after flush"
    );
}

fn walk_and_collect_parquet(root: &std::path::Path, out: &mut std::collections::HashSet<String>) {
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for entry in rd.flatten() {
                    stack.push(entry.path());
                }
            }
        } else if p.extension().and_then(|s| s.to_str()) == Some("parquet") {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                out.insert(name.clone()),
                "duplicate part name detected: {name}"
            );
        }
    }
}
