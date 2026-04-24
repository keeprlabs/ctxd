//! When the on-disk HNSW graph file is corrupted, the index must
//! recover by rebuilding from `vector_embeddings`. We force this by
//! truncating the `.hnsw.graph` file (and similarly the `.meta`
//! sidecar — different code path) and asserting the rebuild
//! reconstructs every event_id.

use ctxd_store_sqlite::views::vector::VectorIndexConfig;
use ctxd_store_sqlite::EventStore;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

fn cfg() -> VectorIndexConfig {
    VectorIndexConfig {
        dimensions: 8,
        flush_every_n_inserts: 100,
        max_elements: 1024,
        max_nb_layers: 16,
    }
}

fn rand_vec(seed: u64) -> Vec<f32> {
    let mut x = seed
        .wrapping_mul(2862933555777941757)
        .wrapping_add(3037000493);
    let mut out = Vec::with_capacity(8);
    for _ in 0..8 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (x >> 32) as u32 as f32 / u32::MAX as f32;
        out.push(f - 0.5 + 1e-3);
    }
    out
}

#[tokio::test]
async fn corrupt_meta_file_triggers_rebuild() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");

    // Phase 1: seed.
    let mut event_ids = Vec::new();
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        for i in 0..30u64 {
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

    // Phase 2: corrupt the meta sidecar — overwrite with junk so
    // the magic-number check fails. Don't delete it (different
    // code path); we want to test the magic check specifically.
    let meta_path = tmp.path().join("ctxd.db.hnsw.meta");
    let mut f = fs::File::create(&meta_path).unwrap();
    f.write_all(b"NOT THE RIGHT MAGIC BYTES AT ALL").unwrap();
    drop(f);

    // Phase 3: reopen — must rebuild from SQL.
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        assert_eq!(idx.len(), 30, "rebuild must restore all 30 vectors");
        for (i, want) in event_ids.iter().enumerate() {
            let q = rand_vec(i as u64);
            let r = idx.search(&q, 1).unwrap();
            assert_eq!(r[0].0, *want);
        }
    }
}

#[tokio::test]
async fn corrupt_graph_file_triggers_rebuild() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");

    let mut event_ids = Vec::new();
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        for i in 0..30u64 {
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

    // Truncate the graph file. hnsw_rs::load_hnsw will panic / err
    // on a short read; we expect the error path to be caught and
    // converted to a rebuild.
    let graph_path = tmp.path().join("ctxd.db.hnsw.graph");
    let mut f = fs::File::create(&graph_path).unwrap();
    f.write_all(&[0u8; 16]).unwrap();
    drop(f);

    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        assert_eq!(idx.len(), 30, "rebuild after graph corruption");
        for (i, want) in event_ids.iter().enumerate() {
            let q = rand_vec(i as u64);
            let r = idx.search(&q, 1).unwrap();
            assert_eq!(r[0].0, *want);
        }
    }
}

#[tokio::test]
async fn missing_map_file_triggers_rebuild() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");

    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        for i in 0..15u64 {
            let event_id = format!("evt-{i:03}");
            let v = rand_vec(i);
            store
                .vector_upsert_impl(&event_id, "test-model", &v)
                .await
                .unwrap();
            idx.upsert(&event_id, &v).unwrap();
        }
        idx.flush().unwrap();
    }

    // Delete the map file — the graph + data + meta exist but we
    // can't reconstruct id<->event without the map.
    let map_path = tmp.path().join("ctxd.db.hnsw.map");
    fs::remove_file(&map_path).unwrap();

    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        assert_eq!(idx.len(), 15);
    }
}
