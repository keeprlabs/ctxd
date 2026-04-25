//! Recursive-read latency smoke test. 1000 events across varied
//! subjects under `/work/<n>`, with a sibling tree `/home/<n>` that
//! must not contaminate the result. We assert the p99 latency of a
//! recursive read is under 500 ms — looser than Postgres's tsvector
//! bound because this backend scans Parquet column-by-column.

use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_duckobj::DuckObjStore;
use std::time::Instant;
use tempfile::TempDir;

#[tokio::test]
async fn recursive_read_p99_under_500ms() {
    let td = TempDir::new().unwrap();
    let root = td.path().to_path_buf();
    let store = DuckObjStore::open_local(&root).await.unwrap();

    // Seed 1000 under /work, 500 under /home.
    for i in 0..1000 {
        let s = Subject::new(&format!("/work/team{}/proj{}", i % 10, i % 7)).unwrap();
        let e = Event::new(
            "ctxd://test".to_string(),
            s,
            "demo".to_string(),
            serde_json::json!({"i": i, "note": "ongoing"}),
        );
        store.append(e).await.unwrap();
    }
    for i in 0..500 {
        let s = Subject::new(&format!("/home/node{}", i % 5)).unwrap();
        let e = Event::new(
            "ctxd://test".to_string(),
            s,
            "demo".to_string(),
            serde_json::json!({"i": i}),
        );
        store.append(e).await.unwrap();
    }
    store.flush().await.unwrap();

    // Measure 20 reads; compute the tail latency. p99 of 20 is the
    // max sample.
    let work = Subject::new("/work").unwrap();
    let mut samples = Vec::with_capacity(20);
    let mut counts = Vec::with_capacity(20);
    for _ in 0..20 {
        let t0 = Instant::now();
        let events = store.read(&work, true).await.unwrap();
        let dur = t0.elapsed();
        samples.push(dur);
        counts.push(events.len());
    }

    for c in &counts {
        assert_eq!(
            *c, 1000,
            "recursive read should return exactly 1000 /work/**"
        );
    }

    let p99 = samples.iter().max().copied().unwrap();
    assert!(
        p99.as_millis() < 500,
        "p99 recursive-read latency exceeded 500ms: {:?}",
        p99
    );
}
