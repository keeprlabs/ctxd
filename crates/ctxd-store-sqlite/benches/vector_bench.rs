//! Vector-search latency benchmarks at N=10k embeddings.
//!
//! We compare:
//! 1. Pure FTS search latency (baseline).
//! 2. Pure vector search via the persisted HNSW index.
//! 3. The combined FTS + vector cost that hybrid search pays.
//!
//! All numbers feed `docs/benchmark-results.md`.

use criterion::{criterion_group, criterion_main, Criterion};
use ctxd_core::event::Event;
use ctxd_core::subject::Subject;
use ctxd_store_sqlite::views::vector::{VectorIndex, VectorIndexConfig};
use ctxd_store_sqlite::EventStore;

/// 64-dim seems to be the sweet spot for benchmarks: high enough to
/// be representative, low enough that we don't accidentally measure
/// memory allocator throughput.
const DIMS: usize = 64;
const N: usize = 10_000;

fn det_vec(seed: u64) -> Vec<f32> {
    let mut x = seed
        .wrapping_mul(2862933555777941757)
        .wrapping_add(3037000493);
    let mut out = Vec::with_capacity(DIMS);
    for _ in 0..DIMS {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (x >> 32) as u32 as f32 / u32::MAX as f32;
        out.push(f - 0.5 + 1e-3);
    }
    out
}

fn cfg() -> VectorIndexConfig {
    VectorIndexConfig {
        dimensions: DIMS,
        flush_every_n_inserts: 100_000, // disable mid-bench flushes
        max_elements: N + 1024,
        max_nb_layers: 16,
    }
}

fn bench_vector(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let store = rt.block_on(async {
        let store = EventStore::open_memory().await.unwrap();
        for i in 0..N {
            let event = Event::new(
                "ctxd://bench".to_string(),
                Subject::new(&format!("/bench/vec/item{i}")).unwrap(),
                "doc".to_string(),
                serde_json::json!({"content": format!("searchable doc number {i}")}),
            );
            let stored = store.append(event).await.unwrap();
            let v = det_vec(i as u64);
            store
                .vector_upsert_impl(&stored.id.to_string(), "bench-model", &v)
                .await
                .unwrap();
        }
        store
    });

    // Build a standalone in-memory HNSW index over the same vectors.
    let idx = VectorIndex::open_in_memory(cfg());
    for i in 0..N {
        idx.upsert(&format!("evt-{i}"), &det_vec(i as u64)).unwrap();
    }

    c.bench_function("vector_search_hnsw_k10_n10k", |b| {
        b.iter(|| {
            let q = det_vec(42);
            let _ = idx.search(&q, 10).unwrap();
        });
    });

    // Brute-force scan via Store::vector_search_impl (no index attached).
    c.bench_function("vector_search_brute_k10_n10k", |b| {
        b.iter(|| {
            rt.block_on(async {
                let q = det_vec(42);
                let _ = store.vector_search_impl(&q, 10).await.unwrap();
            });
        });
    });

    // FTS baseline at N=10k.
    c.bench_function("fts_search_n10k_baseline", |b| {
        b.iter(|| {
            rt.block_on(async {
                let _ = store.search("searchable", Some(10)).await.unwrap();
            });
        });
    });

    // Hybrid cost = FTS + vector. We measure them sequentially in
    // the same closure to capture the realistic combined cost RRF
    // pays before its constant-time merge.
    c.bench_function("hybrid_fts_plus_vector_k10_n10k", |b| {
        b.iter(|| {
            let q = det_vec(42);
            let _ = idx.search(&q, 10).unwrap();
            rt.block_on(async {
                let _ = store.search("searchable", Some(10)).await.unwrap();
            });
        });
    });
}

criterion_group!(benches, bench_vector);
criterion_main!(benches);
