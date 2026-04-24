//! Simulate a crash: insert N vectors, drop the index without
//! flushing, reopen and confirm the rebuild path picks up the
//! stragglers from `vector_embeddings`.

use ctxd_store_sqlite::views::vector::VectorIndexConfig;
use ctxd_store_sqlite::EventStore;
use tempfile::TempDir;

fn cfg() -> VectorIndexConfig {
    VectorIndexConfig {
        dimensions: 8,
        // Larger than 500 so no implicit flushes happen during
        // the "before crash" phase — we want to lose ALL writes
        // from the in-memory index.
        flush_every_n_inserts: 100_000,
        max_elements: 8192,
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
async fn lost_writes_are_recovered_from_sql() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("ctxd.db");

    // Phase 1: write 500 vectors into SQL + the index, do NOT flush.
    {
        let mut store = EventStore::open(&path).await.unwrap();
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        for i in 0..500u64 {
            let event_id = format!("evt-{i:04}");
            let v = rand_vec(i);
            store
                .vector_upsert_impl(&event_id, "test-model", &v)
                .await
                .unwrap();
            idx.upsert(&event_id, &v).unwrap();
        }
        // Simulate a kill -9: drop everything without flushing.
        // (Letting `store` drop would just release the connection
        // pool — no implicit flush in our code paths.)
        drop(idx);
        drop(store);
    }

    // Phase 2: reopen, write 100 more, ensure all 600 are searchable.
    {
        let mut store = EventStore::open(&path).await.unwrap();
        // The .hnsw.* sidecars don't exist yet, so this rebuild
        // pulls all 500 from SQL.
        let idx = store.ensure_vector_index(cfg()).await.unwrap();
        assert_eq!(idx.len(), 500);
        for i in 500..600u64 {
            let event_id = format!("evt-{i:04}");
            let v = rand_vec(i);
            store
                .vector_upsert_impl(&event_id, "test-model", &v)
                .await
                .unwrap();
            idx.upsert(&event_id, &v).unwrap();
        }
        assert_eq!(idx.len(), 600);

        // Sample a few — query their own seed vector and assert
        // self is the nearest neighbor.
        for i in [0u64, 250, 499, 500, 599] {
            let want = format!("evt-{i:04}");
            let q = rand_vec(i);
            let r = idx.search(&q, 1).unwrap();
            assert_eq!(r[0].0, want, "expected {want} as nearest");
        }
    }
}
