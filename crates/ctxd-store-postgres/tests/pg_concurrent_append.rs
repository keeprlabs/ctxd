//! Concurrent appenders on distinct subjects must not deadlock or
//! lose writes, and the per-subject hash chain must remain valid.

use ctxd_core::event::Event;
use ctxd_core::hash::PredecessorHash;
use ctxd_core::subject::Subject;
use ctxd_store_postgres::PostgresStore;
use sqlx::Executor;
use std::sync::Arc;

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

#[tokio::test]
async fn concurrent_appenders_on_distinct_subjects_keep_chains_valid() {
    if pg_url_or_skip("concurrent_appenders").is_none() {
        return;
    }
    let store = Arc::new(fresh_store().await);

    let mut handles = Vec::new();
    for task_id in 0..10 {
        let s = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            for i in 0..50 {
                let subj = Subject::new(&format!("/concurrent/task{task_id}")).unwrap();
                let evt = Event::new(
                    "ctxd://test".to_string(),
                    subj,
                    "demo".to_string(),
                    serde_json::json!({"task": task_id, "i": i}),
                );
                s.append(evt).await.expect("append must succeed");
            }
        }));
    }
    for h in handles {
        h.await.expect("task must complete");
    }

    // Total should be 500 events under /concurrent.
    let root = Subject::new("/concurrent").unwrap();
    let all = store.read(&root, true).await.expect("read recursive");
    assert_eq!(all.len(), 500);

    // Each task's hash chain must verify.
    for task_id in 0..10 {
        let subj = Subject::new(&format!("/concurrent/task{task_id}")).unwrap();
        let events = store.read(&subj, false).await.expect("read");
        assert_eq!(
            events.len(),
            50,
            "task {task_id} should have 50 events but has {}",
            events.len()
        );
        for i in 1..events.len() {
            let expected = PredecessorHash::compute(&events[i - 1])
                .expect("hash predecessor")
                .to_string();
            assert_eq!(
                events[i].predecessorhash.as_deref(),
                Some(expected.as_str()),
                "hash chain broken at task {task_id} event {i}"
            );
        }
    }
}
