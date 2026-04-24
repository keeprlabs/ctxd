//! Recursive read performance under realistic prefix density.
//!
//! Inserts 1k events under `/work/a/...` and asserts that reading the
//! whole tree completes well within the conformance budget. With the
//! trigram index in place this is sub-50ms on a laptop; without it,
//! the btree index also serves anchored LIKE patterns efficiently
//! because PostgreSQL optimizes `LIKE 'literal%'` into a range scan.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
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

#[tokio::test]
async fn recursive_read_over_1k_subjects() {
    if pg_url_or_skip("recursive_read_over_1k_subjects").is_none() {
        return;
    }
    let store = fresh_store().await;

    // 1k events under /work/a/<bucket>/<i>. Different prefix families
    // so the recursive read has to walk the whole subtree.
    for i in 0..1_000 {
        let bucket = i % 100;
        let subj = Subject::new(&format!("/work/a/{bucket}/item{i}")).unwrap();
        let evt = Event::new(
            "ctxd://test".to_string(),
            subj,
            "demo".to_string(),
            serde_json::json!({"i": i}),
        );
        store.append(evt).await.expect("append");
    }

    // Throw in 100 events under /work/b to make sure we *don't* match.
    for i in 0..100 {
        let subj = Subject::new(&format!("/work/b/{i}")).unwrap();
        let evt = Event::new(
            "ctxd://test".to_string(),
            subj,
            "demo".to_string(),
            serde_json::json!({"i": i}),
        );
        store.append(evt).await.expect("append");
    }

    // Recursive read against /work/a should return exactly 1k events.
    let root = Subject::new("/work/a").unwrap();
    let started = std::time::Instant::now();
    let events = store.read(&root, true).await.expect("read recursive");
    let elapsed = started.elapsed();
    assert_eq!(events.len(), 1_000, "expected 1k matching events");

    // Performance guard. Generous bound because CI can be slow.
    assert!(
        elapsed < std::time::Duration::from_millis(2_000),
        "recursive read took {elapsed:?}, expected < 2s"
    );
}
