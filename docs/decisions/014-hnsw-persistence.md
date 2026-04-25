# ADR 014: HNSW persistence + crash recovery

Status: Accepted (v0.3 Phase 4B)
Date: 2026-04-24

## Context

ctxd v0.2 shipped an in-memory HNSW vector index using
`instant-distance`. The index rebuilt the entire graph on every
insert, which is fine at hundreds of vectors and intolerable at
tens of thousands. v0.3 needs:

1. Sub-millisecond k-NN at 10k+ vectors.
2. Survival across daemon restarts.
3. Recovery from torn writes / corrupt files.
4. Concurrent reads and exclusive writes.

## Decision

Migrate to `hnsw_rs` 0.3.4, add a thin persistence wrapper, and
treat the on-disk graph as a *materialized view* whose source of
truth is the `vector_embeddings` SQLite table.

### HNSW parameters

We pin:

- **M = 16.** The HNSW paper's recommended default for "general
  workloads." Larger M improves recall but slows build + search;
  smaller M makes the graph too sparse for k>20.
- **ef_construction = 200.** Build-time accuracy knob. 200 is the
  knee on the recall-vs-build-time curve at our expected ≤1M
  corpus sizes.
- **ef_search = 50.** Query-time accuracy knob. 50 keeps p99
  latency under 1 ms at N=10k while preserving >0.99 recall@10.
- **max_nb_layers = 16.** Hard requirement: `hnsw_rs` 0.3 rejects
  dumps where `nb_layer != NB_LAYER_MAX (=16)`. We surface this
  in the `VectorIndexConfig` rustdoc so it cannot silently regress.

Options considered:

- **Faiss-style IVF.** Better recall on very large corpora, worse
  insert latency, requires periodic re-clustering — overkill for
  v0.3's single-node story.
- **Annoy.** No incremental inserts after build. Disqualified.
- **Pure Rust port via `hnswlib-rs`.** Younger crate, less battle
  tested. Revisit in v0.5+ if `hnsw_rs` becomes a maintenance
  burden.

### On-disk layout

For a database file at `<db>` we write four sidecars:

| File | Owner | Format |
|------|-------|--------|
| `<db>.hnsw.graph` | `hnsw_rs::file_dump` | binary adjacency lists |
| `<db>.hnsw.data` | `hnsw_rs::file_dump` | raw vector payloads |
| `<db>.hnsw.meta` | ctxd | `b"CTXDVEC1"` + version byte + JSON {dim, element_count} |
| `<db>.hnsw.map` | ctxd | JSON `Vec<String>` of internal-id → event_id |

The meta sidecar carries our magic header + version byte. A torn
write or version mismatch fails the magic check and triggers a
rebuild from `vector_embeddings`. The map sidecar is required
because `hnsw_rs` only stores integer ids; we own the
event-id ↔ internal-id mapping.

### Persistence cadence

`flush()` (= `Hnsw::file_dump` + meta + map) runs:

1. On graceful shutdown via `VectorIndex::flush`.
2. Every `flush_every_n_inserts` (default 1000) inserts.

Between flushes, an unclean shutdown loses recent inserts from the
on-disk graph. Recovery runs at `EventStore::ensure_vector_index`:

```text
graph element_count != SQL row count
  -> rebuild_from(SELECT event_id, vector FROM vector_embeddings)
```

The rebuild is `O(N log N)` thanks to HNSW; at 10k vectors it
finishes in ~5 ms (single-threaded). At 100k vectors we surface
progress every 10k entries via tracing so an operator watching
`ctxd.log` knows the daemon hasn't hung.

### Concurrent access

`hnsw_rs` is internally synchronized for inserts via parking_lot,
and search is concurrent-safe. We wrap the `Hnsw` plus our own
`id_to_event` map in a `std::sync::RwLock` to keep them in
lockstep — without it, a query could observe a vector before its
event_id mapping was updated.

### Pre-validation of the graph file

`hnsw_rs::HnswIo::init` internally `.unwrap()`s `load_description`,
so feeding it a malformed graph file *panics inside the library*
instead of returning a `Result`. We added a 4-byte magic check
before invoking `load_hnsw` so corruption is caught cleanly and
routed to the rebuild path.

### Lifetime hack

`load_hnsw` returns `Hnsw<'a, ...>` where `'a` borrows from the
`HnswIo` loader. Our `VectorIndex` holds the graph as `'static`
because we need to store it across an async boundary. We
`Box::leak` the loader on the rare load path so the borrow
satisfies `'static`. There is at most one leak per process per
index instance (reloads happen on startup and after rebuild) —
acceptable.

## Consequences

- HNSW now persists; restart no longer requires a full rebuild
  in the common case.
- Brute-force cosine scan is still available via
  `vector_search_impl` when the index hasn't been opened — useful
  for the conformance suite and small stores.
- Disk footprint: ~`(M × 16 + dim × 4) bytes/vector`. At 10k
  vectors × 64 dim that's roughly 5 MB — negligible vs SQLite.
- We're now coupled to `hnsw_rs`'s on-disk format. Bumping its
  major version requires bumping `META_VERSION` + a one-time
  rebuild on first startup.

## Revisit when

- Corpus exceeds 1M vectors (HNSW's default `max_elements`).
  Either bump the config or move to disk-resident IVF.
- `hnsw_rs` releases a stable 1.0 with breaking changes — check
  whether `load_hnsw` still requires the lifetime dance.
- We need vector deletion. HNSW doesn't natively support
  removal; today we tolerate stale entries and dedupe by
  event_id at search time. A future tombstone table + periodic
  rebuild would tighten this.
