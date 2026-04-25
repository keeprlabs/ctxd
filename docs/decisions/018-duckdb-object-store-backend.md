# ADR 018: DuckDB-on-object-store Backend

## Context

ctxd already has two backends behind the `Store` trait:

- `ctxd-store-sqlite` — single-binary, embedded, transactional. Default for laptops and small teams.
- `ctxd-store-postgres` — clustered, OLTP-shaped, scales horizontally on writes.

Neither is ideal for the analytics-heavy "context-as-warehouse" use case: months of events, ad-hoc temporal queries, append-only retention. Postgres tables grow without bound; SQLite hits IO ceilings on a laptop. We want a third option that costs ~free at scale, plays well with object storage, and lets analysts run BI-style queries without a sidecar ETL.

## Decision

Add `ctxd-store-duckobj`: an append-only Parquet log on an object store, with a local SQLite sidecar for small transactional state.

### Architecture

```
<store-uri>/
  events/<subject_root>/<yyyy-mm>/part-<seq>-<uuid>.parquet
  events/<subject_root>/_manifest.json
  events/_index/by_id.parquet         # secondary id → (part, offset) index

<state-dir>/
  events.wal                          # append-only WAL of unflushed events
  sidecar.db                          # SQLite: kv_view, peers, peer_cursors,
                                      #         revoked_tokens, vector_embeddings,
                                      #         token_budgets, pending_approvals,
                                      #         graph_*
```

### Durability model

1. `append()` writes to the in-memory buffer **and** appends to `events.wal` before returning. WAL fsync is the durability boundary.
2. A background flush seals the buffer into a Parquet part when any of these triggers:
   - buffer crosses 1000 events, OR
   - serialized size crosses 16 MB, OR
   - 1 second elapses since last flush, OR
   - explicit `flush()` call.
3. The flush writes `part-<seq>-<uuid>.parquet`, then atomically updates `_manifest.json` (write-temp-then-rename / `copy_if_not_exists` on object stores that support it). The manifest update is the integrity boundary — a part file without a manifest entry is invisible to readers and garbage-collectable by a future `compact` admin tool.
4. After a successful manifest update, `Wal::truncate` resets the WAL to empty.
5. On startup, the WAL is replayed to rehydrate the in-memory buffer so events that landed between an `append` and the next flush survive a crash.

### Read path

- Sealed parts: DuckDB query `SELECT ... FROM read_parquet('<root>/events/<subject_root>/**/*.parquet') WHERE ...`. The list of valid parts comes from the manifest, not raw object-store listing — this is robust to eventually-consistent list APIs and to abandoned partial uploads.
- Unflushed: in-memory buffer scanned in lockstep, results merged with sealed-part hits ordered by `(seq, id)`.
- `read_at(t)` adds `WHERE time <= $t`. Buffer events with `time > $t` are filtered the same way.
- `search` is the weakest method on this backend: minimum-viable text matching via DuckDB's `ILIKE`. Real FTS (DuckDB's FTS module or an external Tantivy index) is a v0.4 enhancement.
- KV / peers / caveats / vectors all hit the SQLite sidecar — same code path as `ctxd-store-sqlite` for those tables, which means LWW semantics match byte-for-byte (essential for federation).

### Why a sidecar SQLite instead of all-DuckDB?

- DuckDB is read-optimized for analytical scans. Single-row UPSERTs (KV-view writes, cursor updates) are slow on Parquet because they require rewriting whole row groups.
- The transactional state is small — KB to MB at most across the deployment lifetime. SQLite handles it cheaply on local disk.
- Federation LWW reuses the existing `kv_view` UPSERT shape from `ctxd-store-sqlite` without behavior drift. Two backends matching the same byte-level invariant matters for cross-peer determinism.
- Backups: the operator backs up the object-store bucket *and* the sidecar SQLite. The sidecar is small enough to copy in seconds.

### Schema

Parquet column layout matches the canonical Event:

| Column | Arrow type | Notes |
|---|---|---|
| `seq` | Int64 | Monotonic, set at append time |
| `id` | FixedSizeBinary(16) | UUIDv7 raw bytes |
| `source` | Utf8 | |
| `subject` | Utf8 | |
| `event_type` | Utf8 | |
| `time` | Timestamp(Nanosecond, UTC) | |
| `datacontenttype` | Utf8 | |
| `data` | Binary | JSON bytes — preserves byte-identical roundtrip |
| `predecessorhash` | Utf8 nullable | |
| `signature` | Utf8 nullable | |
| `parents` | List(FixedSizeBinary(16)) | UUIDv7 raw bytes |
| `attestation` | Binary nullable | TEE proof bytes |
| `specversion` | Utf8 | |

Parquet's RLE / dictionary encoding makes the dense `subject` and `event_type` columns very cheap.

### Rotation policy

- **Size**: 64 MB raw Parquet (compressed). Past this, rotate to a new part.
- **Time**: 5-minute soft ceiling. Idle deployments still get a new part every 5 minutes so the recent window is queryable without scanning the buffer.
- **Subject root partitioning**: events partition by the first path segment so `recursive` reads under `/work/...` only scan parquet files under `events/work/`. This is a coarse partition; deeper partitioning is a v0.4 tunable.

### Object store backends

The `object_store` crate (Apache Arrow project) abstracts over S3, R2, Azure, GCS, and local filesystem. ctxd uses URI prefixes to dispatch:

- `s3://bucket/prefix` — AWS S3 / R2 (R2 supports the S3 API).
- `file:///abs/path` — local filesystem (and bare paths are coerced).
- `az://...`, `gs://...` — cloud blob.

Eventually-consistent list APIs are not a problem because the manifest is the source of truth.

## Consequences

### Pros

- Cheapest scale economics — Parquet on object storage is essentially free per GB-month.
- Analytical queries via DuckDB run in seconds over months of context (column scans + predicate pushdown).
- Append-only matches the event-log invariant exactly.
- WAL keeps the durability story tight.

### Cons

- Read-your-writes requires merging the buffer with sealed parts — small extra complexity in every read path.
- Search is weak in v0.3. Users who need search-heavy workloads should run with `--storage sqlite` or `--storage postgres`.
- Partial Parquet uploads to object storage need cleanup. v0.3 ignores them; a future `compact` tool will sweep.
- DuckDB binary footprint adds ~10 MB to the daemon when this backend is enabled.

### Revisit when

- We need real FTS on this backend → wire DuckDB's `fts` extension or attach a Tantivy index.
- Buffered-but-unflushed events become a hot path → move to a per-subject_root buffer and parallelize flushes.
- Cross-region replication latency matters → switch from per-event Parquet rows to micro-batch streaming.
