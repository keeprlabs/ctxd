# Storage: DuckDB-on-object-store

The `ctxd-store-duckobj` backend keeps the event log as append-only Parquet files on an object store, with a small SQLite sidecar for transactional state (KV view, peers, caveats, vector embeddings). Picks up where SQLite hits IO ceilings and Postgres becomes operationally heavy.

For the architecture details see [ADR 018](decisions/018-duckdb-object-store-backend.md).

## When to use this backend

| Workload | Recommended backend |
|---|---|
| Laptop / single-machine, dev | `sqlite` (default) |
| OLTP cluster, search-heavy | `postgres` |
| Months-of-events archive, analytics | `duckdb-object` |
| Multi-tenant SaaS with cheap retention | `duckdb-object` (S3 + lifecycle rules) |

If you need real-time MCP transports, wire protocol, or federation in v0.3 — use `sqlite`. Those paths require concrete `EventStore` plumbing in v0.3 (see [ADR 019](decisions/019-storage-selector.md)) and run in a "minimal HTTP admin" mode under `--storage duckdb-object` until v0.4.

## Build with this backend enabled

```bash
cargo build --features storage-duckdb-object
# or for both Postgres + DuckDB
cargo build --features storage-postgres,storage-duckdb-object
```

Default builds do not include this backend — the deps (`arrow`, `parquet`, `duckdb`, `object_store`) add ~10 MB to the binary.

## URI scheme

`--storage-uri` accepts:

- `file:///abs/path` — local filesystem. Best for testing and small single-host deployments.
- `s3://bucket/prefix` — AWS S3. Authenticated via the standard AWS env (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`) or an IAM role.
- `s3://bucket/prefix` with `AWS_ENDPOINT_URL` set — Cloudflare R2, MinIO, or any S3-compatible store.
- `az://container/prefix` — Azure Blob Storage.
- `gs://bucket/prefix` — Google Cloud Storage.

The sidecar SQLite is always local. Its path defaults to `<state-dir>/sidecar.db`; override with `--state-dir`.

## Local example

```bash
mkdir -p /tmp/ctxd-events
ctxd serve \
  --storage duckdb-object \
  --storage-uri file:///tmp/ctxd-events \
  --bind 127.0.0.1:7777
```

In another terminal:

```bash
curl -s -X POST http://127.0.0.1:7777/v1/append \
  -H 'content-type: application/json' \
  -d '{"subject":"/work/acme/notes","type":"ctx.note","data":{"content":"hello"}}'

curl -s "http://127.0.0.1:7777/v1/read?subject=/work&recursive=true" | jq .
```

## S3 / R2 example

```bash
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
export AWS_REGION=us-east-1
ctxd serve \
  --storage duckdb-object \
  --storage-uri s3://ctxd-prod/events \
  --bind 0.0.0.0:7777
```

For R2 set `AWS_ENDPOINT_URL=https://<account-id>.r2.cloudflarestorage.com` and use a bucket created in your R2 namespace.

## On-disk layout

Under `<storage-uri>/`:

```
events/
  work/
    2026-04/
      part-000001-7d4f...uuid.parquet
      part-000002-91ab...uuid.parquet
      _manifest.json
    2026-05/
      part-000003-1cde...uuid.parquet
      _manifest.json
  personal/
    ...
  _index/
    by_id.parquet
```

Under `<state-dir>/`:

```
events.wal           # unflushed events; rotated to empty after each successful flush
sidecar.db           # SQLite: kv_view, peers, peer_cursors, revoked_tokens,
                     #         vector_embeddings, token_budgets, pending_approvals,
                     #         graph_entities, graph_relationships
```

## Rotation

A new Parquet part is written when any of these triggers:

- 1000 events buffered, OR
- 16 MB serialized in the buffer, OR
- 1 second elapsed since the last flush, OR
- `flush()` called explicitly.

A new directory partition (`<yyyy-mm>/`) opens at month boundaries.

Configurable knobs (in code, not yet CLI-exposed in v0.3):

- `flush_max_events: usize`
- `flush_max_bytes: usize`
- `flush_max_idle: Duration`
- `rotation_max_part_bytes: usize` (default 64 MB)

## Backups

Two things to back up:

1. The object-store bucket. Use the cloud's native cross-region replication or lifecycle rules.
2. The sidecar SQLite (`<state-dir>/sidecar.db`). Small enough to copy on a schedule. Lose this and you re-derive the KV / peers state from the event log on next startup.

The WAL is **not** part of a backup. It contains in-flight events that haven't been promoted to Parquet. On a clean shutdown the WAL is empty.

## Sizing

Rough order-of-magnitude:

| Workload | Events / day | Storage / day (Parquet, compressed) | DuckDB scan time |
|---|---|---|---|
| 1 user, knowledge worker | 5k | 1–5 MB | < 1s |
| 100 users, team | 500k | 100–500 MB | 1–3s |
| 10k users, SaaS tenant | 50M | 10–50 GB | 5–30s |

DuckDB's column-store + RLE compression makes typical CRM/notes workloads fit in single-digit GB per million events.

## Limitations in v0.3

- No FTS. `search` falls back to `ILIKE` over the JSON-encoded `data` column. Use `--storage postgres` (tsvector) or `--storage sqlite` (FTS5) if you need real search.
- No federation, MCP transports, or wire protocol. Minimal HTTP admin only — see ADR 019.
- No automatic compaction of orphaned Parquet parts (parts that exist on the object store but aren't in the manifest). v0.4 will ship `ctxd compact`.
- No pgvector-style native vector index. Vector search uses brute-force cosine over the SQLite sidecar (matching the Postgres backend's v0.3 limitation).

## Troubleshooting

**"manifest.json out of sync with object store"** — happens when a writer crashed mid-update on an eventually-consistent backend. Restart the daemon; the WAL replay + manifest re-read fixes it. Persistent state of this kind in v0.4's `ctxd compact` will reconcile.

**"part file not in manifest"** — orphaned Parquet from a crashed writer. Invisible to readers; will be cleaned by `ctxd compact`. Safe to delete manually if you're sure no in-flight write is targeting it.

**"DuckDB extension not found: httpfs"** — the `httpfs` extension is loaded at startup. If your environment blocks the extension download, pre-download it and set `DUCKDB_EXTENSION_REPOSITORY` to a local mirror. (DuckDB extension management is upstream — see DuckDB docs.)

**"sidecar.db locked"** — another `ctxd` process is running against the same `--state-dir`. Only one writer at a time per state dir; coordinate with a process manager.
