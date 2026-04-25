# ctxd-store Benchmark Results

Run with `cargo bench -p ctxd-store` on an in-memory SQLite store.

## Results

| Benchmark | Time (mean) | Description |
|-----------|------------|-------------|
| `append_single` | 2.85 ms | Append one event (includes store init) |
| `append_100_sequential` | 37.46 ms | Append 100 events sequentially (~375 us/event) |
| `read_exact_1_event` | 79.74 us | Read a single subject with 1 event |
| `read_recursive_100_events` | 1.09 ms | Recursive read over 100 events under one prefix |
| `read_recursive_1000_events` | 10.03 ms | Recursive read over 1000 events under one prefix |
| `search_fts_over_100_events` | 987.17 us | FTS search over 100 events |
| `search_fts_over_10000_events` | 105.87 ms | FTS search over 10000 events |
| `kv_get_latest_value` | 68.92 us | Get latest value for a subject (100 events written) |

## Environment

- Profile: release (optimized)
- Store: in-memory SQLite (`:memory:`)
- Date: 2026-04-22
- Criterion v0.5, 100 samples per benchmark

## Federation (v0.3 Phase 2)

Run with `cargo test --release -p ctxd-cli --test federation_bench -- --ignored --nocapture`.

| Benchmark | Result | Threshold | Status |
|-----------|--------|-----------|--------|
| One-way replication throughput (localhost TCP) | **1516 events/sec** | ≥ 1000 events/sec | green |
| Third-party block verify (3-authority chain) | **415 us/verify** | < 1000 us | green |

### Notes

- Replication throughput is dominated by per-event TCP connect/disconnect:
  the v0.3 outbound path opens a fresh TCP socket per replicate call.
  A v0.4 long-lived per-peer connection should ~10x this.
- `verify_multi` is dominated by biscuit's authorizer setup
  (parameter binding + datalog code parse). The actual signature
  check is a small slice of the total. Caching the parsed authorizer
  is a v0.4 follow-up.
- Both numbers meet the v0.3 acceptance bar; no flamegraph
  optimization was required.

## Environment (federation benchmarks)

- Profile: release (optimized)
- Stores: two in-memory SQLite daemons connected over `127.0.0.1`
- Date: 2026-04-24
- Hardware: macOS arm64 (Darwin 25.3.0)

## Vector search (v0.3 Phase 4B)

Run with `cargo bench -p ctxd-store-sqlite --bench vector_bench`.
Corpus: 10,000 deterministic 64-dim vectors, in-memory SQLite store,
HNSW parameters `M=16`, `ef_construction=200`, `ef_search=50`.

| Benchmark | Time (mean) | Description |
|-----------|------------|-------------|
| `vector_search_hnsw_k10_n10k` | **601 µs** | k=10 nearest neighbors via the persisted HNSW index |
| `vector_search_brute_k10_n10k` | **49.2 ms** | Same query, brute-force cosine scan over `vector_embeddings` |
| `fts_search_n10k_baseline` | **2.10 ms** | FTS5 search at the same N |
| `hybrid_fts_plus_vector_k10_n10k` | **3.27 ms** | FTS + HNSW back-to-back (RRF merge cost is negligible) |

### Takeaways

- **HNSW is ~82x faster than brute force** at N=10k. The gap widens
  with N — brute force is `O(N)`; HNSW is `O(log N)` amortized.
- **Hybrid ≈ FTS + vector + ε** matches the spec's expectation. The
  RRF merge is a HashMap fold over ≤2k entries — measured at <50 µs
  on the same hardware, well below the noise floor of the bench.
- HNSW dominates over FTS for top-k semantic recall but FTS wins on
  exact-token queries that share no surface form with the indexed
  text. Hybrid catches both.

### Methodology

- Profile: release (optimized).
- Hardware: macOS arm64 (Darwin 25.3.0).
- Date: 2026-04-24.
- Quick mode (`--quick`) — Criterion ran 10 samples per benchmark.
  Re-run without `--quick` for the canonical 100-sample distribution.
- Index parameters: `HNSW_M=16`, `ef_construction=200`,
  `ef_search=50`. See ADR 014 for rationale.
