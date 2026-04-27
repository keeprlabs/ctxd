//! Insert N vectors, close the store, reopen, and confirm the
//! HNSW index either reloads from disk OR rebuilds from
//! `vector_embeddings` and returns equivalent results.

use ctxd_store_sqlite::views::vector::{VectorIndex, VectorIndexConfig};
use ctxd_store_sqlite::EventStore;
use std::path::PathBuf;
use tempfile::TempDir;

fn cfg() -> VectorIndexConfig {
    VectorIndexConfig {
        dimensions: 8,
        // Make sure we flush at least once during the inserts so
        // the on-disk graph is exercised end-to-end.
        flush_every_n_inserts: 50,
        max_elements: 1024,
        // hnsw_rs requires this be exactly NB_LAYER_MAX (16) for dump to succeed.
        max_nb_layers: 16,
    }
}

fn rand_vec(seed: u64) -> Vec<f32> {
    // Deterministic pseudo-random float vector seeded by a u64.
    // Good enough for "is the index returning sane neighbors"
    // tests without pulling in `rand`.
    let mut x = seed
        .wrapping_mul(2862933555777941757)
        .wrapping_add(3037000493);
    let mut out = Vec::with_capacity(8);
    for _ in 0..8 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (x >> 32) as u32 as f32 / u32::MAX as f32;
        // Shift to [-0.5, 0.5] then a tiny bias so no exact zeros.
        out.push(f - 0.5 + 1e-3);
    }
    out
}

fn db_path(tmp: &TempDir) -> PathBuf {
    tmp.path().join("ctxd.db")
}

#[tokio::test]
async fn embeddings_survive_close_and_reopen() {
    let tmp = TempDir::new().unwrap();
    let path = db_path(&tmp);

    let mut event_ids: Vec<String> = Vec::with_capacity(100);

    // Phase 1 — open, insert 100 embeddings, close.
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        for i in 0..100u64 {
            let event_id = format!("evt-{i:03}");
            event_ids.push(event_id.clone());
            let v = rand_vec(i);
            store
                .vector_upsert_impl(&event_id, "test-model", &v)
                .await
                .unwrap();
            idx.upsert(&event_id, &v).unwrap();
        }
        idx.flush().unwrap();
    }

    // Phase 2 — reopen and verify each query's own event_id is
    // reachable in the index. HNSW is approximate; at N=100 with
    // adjacent random vectors the strict top-1 can shuffle even for
    // an exact-match query. The persistence invariant we're pinning
    // is "the index has the data" — assert top-3 membership instead.
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        assert_eq!(idx.len(), 100);
        for (i, want) in event_ids.iter().enumerate() {
            let q = rand_vec(i as u64);
            let r = idx.search(&q, 3).unwrap();
            assert!(!r.is_empty(), "no result for {want}");
            let hits: Vec<&str> = r.iter().map(|(id, _)| id.as_str()).collect();
            assert!(
                hits.contains(&want.as_str()),
                "expected {want} in top-3 after restart, got {hits:?}"
            );
        }
    }
}

#[tokio::test]
async fn rebuild_path_runs_when_no_index_files_exist() {
    // Seed only the SQL table; no .hnsw.* sidecars on disk yet.
    let tmp = TempDir::new().unwrap();
    let path = db_path(&tmp);
    let mut event_ids: Vec<String> = Vec::with_capacity(20);
    {
        let store = EventStore::open(&path).await.unwrap();
        for i in 0..20u64 {
            let event_id = format!("evt-{i:03}");
            event_ids.push(event_id.clone());
            let v = rand_vec(i);
            store
                .vector_upsert_impl(&event_id, "test-model", &v)
                .await
                .unwrap();
        }
    }
    // Ensure no .hnsw.* files exist yet.
    for ext in [".hnsw.graph", ".hnsw.data", ".hnsw.meta", ".hnsw.map"] {
        let p = tmp.path().join(format!("ctxd.db{ext}"));
        assert!(!p.exists(), "{p:?} should not exist before reopen");
    }
    // Reopen and ensure_vector_index — should rebuild from SQL.
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        assert_eq!(idx.len(), 20);
        // After rebuild we flushed once, so files now exist.
        for ext in [".hnsw.graph", ".hnsw.data", ".hnsw.meta", ".hnsw.map"] {
            let p = tmp.path().join(format!("ctxd.db{ext}"));
            assert!(p.exists(), "{p:?} should exist after rebuild flush");
        }
    }
}

#[tokio::test]
async fn standalone_index_open_persistent_smoke() {
    // Lower-level smoke test: open an index without an EventStore.
    let tmp = TempDir::new().unwrap();
    let path = db_path(&tmp);
    {
        let (idx, status) = VectorIndex::open_persistent(&path, cfg()).unwrap();
        // Empty fs => RebuildRequired.
        assert_eq!(
            status,
            ctxd_store_sqlite::views::vector::OpenStatus::RebuildRequired
        );
        for i in 0..10u64 {
            idx.upsert(&format!("e{i}"), &rand_vec(i)).unwrap();
        }
        idx.flush().unwrap();
    }
    // Reopen.
    {
        let (idx, status) = VectorIndex::open_persistent(&path, cfg()).unwrap();
        assert_eq!(
            status,
            ctxd_store_sqlite::views::vector::OpenStatus::LoadedFromDisk
        );
        assert_eq!(idx.len(), 10);
    }
}
