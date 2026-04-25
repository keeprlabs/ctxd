# Postgres storage backend

ctxd v0.3 supports PostgreSQL as a first-class storage backend behind
the [`Store`](../crates/ctxd-store-core/src/lib.rs) trait. This is for
operators who want a managed Postgres (RDS, Cloud SQL, Neon, Supabase)
or a multi-tenant deployment that benefits from Postgres' tooling
ecosystem.

ADR 016 documents the schema choices; ADR 017 documents the
conformance pattern that gates every backend.

## When to use this backend

Use Postgres when:

- You already run a managed Postgres and want one fewer datastore.
- You expect > 1M events and want Postgres' query planner working for
  you.
- You need backups, replication, or point-in-time recovery via your
  existing Postgres tooling.
- You have a multi-daemon deployment behind a load balancer.

Stay on SQLite (the default) when:

- You want zero-config single-binary deployment.
- Your event volume is low enough that Postgres' operational cost
  outweighs its benefits.
- You're running on a developer laptop.

## Setup

### 1. Provision the database

Postgres 14 or newer. The `pg_trgm` extension is recommended (not
required) — it accelerates `read --recursive` queries against
`subject LIKE '/prefix/%'`.

If your role has `CREATEDB` and superuser-equivalent on the database,
the migration auto-installs `pg_trgm`. Otherwise have a DBA run, **once
per database**:

```sql
CREATE EXTENSION IF NOT EXISTS pg_trgm;
```

If `pg_trgm` is unavailable, ctxd falls back to a btree index on
`subject` and recursive reads still work — just slower for unanchored
patterns.

### 2. Point ctxd at it

The `--database-url` selector lands in Phase 5C. Until then, callers
construct a `PostgresStore` directly:

```rust
use ctxd_store_postgres::PostgresStore;

let store = PostgresStore::connect("postgres://user:pass@host:5432/ctxd").await?;
```

The connect call applies migrations idempotently — safe to re-run on
every startup.

### 3. Verify

```sql
\dt
```

You should see:

```
 events                 -- the immutable event log
 event_parents          -- parent-edge side table
 kv_view                -- LWW materialized latest-per-subject
 metadata               -- daemon config (root cap key)
 revoked_tokens
 peers                  -- federation peer registry
 peer_cursors           -- replication cursors
 token_budgets          -- BudgetLimit caveat state
 pending_approvals      -- HumanApprovalRequired caveat state
 vector_embeddings      -- raw f32 LE vectors for HNSW
 graph_entities         -- entity-relationship view
 graph_relationships
```

## Recommended instance sizing

| Workload | RDS / Cloud SQL preset | Notes |
|---|---|---|
| < 100k events, single team | db.t3.micro / db-f1-micro | The free-tier shapes are fine; Postgres' overhead is dominated by connection management, not query work |
| 100k–10M events, multi-team | db.r5.large / db-custom-2-7680 | 8 GB RAM is enough for the working set if you avoid wide JSONB queries |
| 10M+ events | db.r5.2xlarge or larger | Consider partitioning `events` by `time` once you cross 10M rows; PostgreSQL handles ranges well but the planner cost goes up |

The dominant resource is **disk I/O for the FTS GIN index** —
`fts_tsv` is a stored generated column with a GIN index, so every
append rewrites a leaf page. Provision enough IOPS that the GIN
update doesn't block the WAL.

## Backups

ctxd's event log is the source of truth — every materialized view
(`kv_view`, `vector_embeddings`, `graph_entities`,
`graph_relationships`) can be rebuilt from `events`. Backup priorities:

1. **`events`** (and its derived `event_parents`). This is the only
   table you must not lose.
2. `peers` + `peer_cursors`. Federation will resync from these.
3. `revoked_tokens` + `token_budgets` + `pending_approvals`. These
   are operational state — losing them re-opens revoked tokens and
   resets budgets, which is bad but recoverable.

Standard `pg_basebackup` + WAL archiving is sufficient. PITR is
attractive if you're paranoid about a bad migration: the rollback is
"restore to the timestamp before the deploy".

## Migrations

Migrations live in
`crates/ctxd-store-postgres/migrations/000N_*.sql` and are embedded
into the binary at compile time. Every CREATE statement is gated on
`IF NOT EXISTS`, so re-running them is safe (the
`pg_migration_idempotency` test enforces this).

Adding a new migration:

1. Create `migrations/000N_<name>.sql` with idempotent statements
   only.
2. Add the file to the `MIGRATIONS` slice in
   `crates/ctxd-store-postgres/src/schema.rs`.
3. Add a test that exercises the new schema if it changes
   user-visible behavior.

## Concurrency model

`PostgresStore::append` takes a per-subject `pg_advisory_xact_lock`
keyed on a stable 64-bit hash of the subject path. Concurrent
appenders on **different** subjects run in parallel; concurrent
appenders on the **same** subject serialize cleanly without
serialization-failure retries. See ADR 016 for the rationale.

## Running the conformance suite locally

```bash
docker run --rm -d --name pg -p 5432:5432 -e POSTGRES_PASSWORD=test postgres:16
CTXD_PG_URL=postgres://postgres:test@localhost:5432/postgres \
  cargo test -p ctxd-store-postgres
```

Each test creates a fresh schema (`ctxd_test_<uuidv7>`) so parallel
test runs don't collide. The schemas are not cleaned up automatically
because dropping them adds to test runtime; the throwaway containers
in CI handle cleanup by being thrown away.

## Troubleshooting

**`operator class "gin_trgm_ops" does not exist`** — `pg_trgm` is
installed in a schema not on the connection's `search_path`. The
migration handles this by schema-qualifying the opclass at
runtime; if you see this error you're probably on an older version of
the migration. Update.

**`permission denied to create extension "pg_trgm"`** — your role
lacks the privilege. Have a DBA run
`CREATE EXTENSION pg_trgm;` once per database. Migrations will
continue to succeed.

**Slow recursive reads** — verify the trigram index exists:
`\d events` should show `idx_events_subject_trgm` of type `gin
(subject gin_trgm_ops)`. If it's missing, `pg_trgm` wasn't installed
when the migration ran. Install it and re-run the migration.

**Hash chain violations in logs** — almost always a clock-skew issue:
two daemons writing under the same subject with `time` values that
disagree. Federation expects every emitter to use a synced clock
(NTP / chrony). The events are still durable; only ordering is
affected.
