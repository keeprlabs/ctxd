# 016 — Postgres backend for ctxd

Status: accepted (v0.3 Phase 5A)
Date: 2026-04-24

## Context

ctxd shipped v0.2 with a single SQLite backend. v0.3 introduces a
generic `Store` trait (ADR 017) so multiple backends can coexist behind
the same interface. This ADR captures the schema and concurrency
choices for the first non-SQLite backend, `ctxd-store-postgres`.

The targets:

- A daemon operator with an existing managed Postgres (RDS, Cloud SQL,
  Neon, Supabase) can point ctxd at it without standing up a separate
  data plane.
- The shared conformance suite (`ctxd-store-core::testsuite`) passes
  byte-identically against both backends so we don't introduce
  semantic drift between them.

## Decisions

### Schema mirrors SQLite, types are Postgres-native

| Concept | SQLite | Postgres |
|---|---|---|
| Event id | `TEXT` (UUID string) | `UUID` |
| Event time | `TEXT` (RFC 3339) | `TIMESTAMPTZ` |
| Event data | `TEXT` (serialized JSON) | `JSONB` |
| Event parents | `TEXT` (comma-separated UUIDs) | `UUID[]` |
| Attestation | `BLOB` | `BYTEA` |
| KV time | `TEXT` | `TIMESTAMPTZ` |
| Public key | `BLOB` | `BYTEA` |
| Trust level | `TEXT` (JSON) | `JSONB` |

Native types let Postgres' query planner see structure (e.g. JSONB
indices, UUID equality joins) without giving up on the round-trip
correctness the conformance suite enforces. The `Event::data`
canonical form is stable JSON (ADR 001), which round-trips through
JSONB byte-for-byte modulo whitespace — and we never compare
serialized forms across backends, only logical values.

### `pg_trgm` GIN index for recursive reads

`read --recursive` translates to `subject = $1 OR subject LIKE '$1/%'`.
Postgres' planner can use a btree index on `subject` for the literal
`subject = $1` term and for an *anchored* prefix LIKE (when the
collation permits), but we provision a `gin_trgm_ops` index in
addition so:

- The planner has a cheap index path for unanchored patterns we may
  add later (e.g. `LIKE '%/work/%'` for cross-tenant search).
- Locale-default collations that don't satisfy btree-prefix
  optimization still get an index path.

The index creation is **schema-qualified at runtime**: the migration
looks up the schema where `gin_trgm_ops` was created and constructs the
DDL with `format(... %I.gin_trgm_ops)`. This matters because operators
running ctxd against a managed Postgres often install extensions in
`public`, and our test isolation creates per-test schemas with
`search_path` overridden — without the qualifier, the index creation
fails with `operator class "gin_trgm_ops" does not exist`.

If the role lacks `CREATE EXTENSION` privilege, the migration's
`CREATE EXTENSION IF NOT EXISTS pg_trgm` will fail. The ops runbook in
`docs/storage-postgres.md` documents the SQL a DBA must run; the rest
of the schema is unaffected and the daemon falls back to btree.

### Per-subject `pg_advisory_xact_lock` for hash-chain TOCTOU

Two appenders on the same subject must serialize: the predecessor hash
of the second event is computed from the *first* event, and racing the
"read last event" with the "insert new event" would yield two events
with the same predecessor.

Options considered:

1. **Serializable transaction** (`SET TRANSACTION ISOLATION LEVEL SERIALIZABLE`).
   Correct but causes serialization failures under contention that the
   client must retry; complicates the trait surface (callers see
   transient errors that don't reflect real conflicts).
2. **Subject-level advisory lock**. Cheap, deterministic, releases on
   transaction commit/rollback. Picked.
3. **Per-row trigger validating predecessorhash**. Pushes correctness
   into the database, but the trigger has to read the previous event
   anyway and re-introduces the same TOCTOU window unless wrapped in
   the same lock.

We use `pg_advisory_xact_lock(BIGINT)` with a stable FNV-1a 64-bit
hash of the subject string. Collisions across distinct subjects only
serialize unrelated appenders — correctness is preserved, throughput
loss is negligible for the populations we anticipate.

### LWW symmetry with SQLite

The KV view's `ON CONFLICT` clause uses the tuple comparison
`(EXCLUDED.time, EXCLUDED.event_id) > (kv_view.time, kv_view.event_id)`
exactly like SQLite. Postgres compares record types lexicographically,
left-to-right — same semantics as SQLite's tuple comparison
(documented in their ROW value section). Federation LWW (ADR 006) is
therefore byte-identical across both backends, and the
`federation_concurrent_writes` integration test that pins this
invariant in the SQLite tree applies unchanged to Postgres.

### No pgvector in v0.3

Vector search uses raw `BYTEA` storage of f32 little-endian floats and
a brute-force cosine scan. This matches the SQLite reference impl and
keeps the conformance suite identical across backends. pgvector is a
v0.4 enhancement (ADR pending) — moving to it requires:

- An optional dependency gated behind a Cargo feature.
- A migration that ALTERs `vector_embeddings.vector` from `BYTEA` to
  `vector(N)`.
- An ANN index (HNSW or IVFFlat) that the conformance suite has to
  account for in result-ordering tolerances.

None of those are blockers for shipping v0.3; deferring keeps the
surface small.

### `LISTEN/NOTIFY` left as a future hook

Postgres' `LISTEN/NOTIFY` would be a natural foundation for per-subject
event subscriptions, but:

- Federation already uses the wire protocol's `PeerReplicate` /
  `PeerCursorRequest` for fan-out.
- The MCP `ctx_subscribe` tool currently delivers via the wire
  protocol's `SUB`, not via Postgres triggers.
- Wiring `LISTEN/NOTIFY` would only matter if multiple ctxd daemons
  share the same Postgres — a topology we haven't validated.

Deferred to v0.4.

## Consequences

- A second backend now exists. Future backends (DuckDB+object store,
  cloud KV) follow the same pattern documented in ADR 017.
- Operators who want Postgres get it; operators who want zero-config
  keep the SQLite default.
- The conformance suite gates every backend identically; backends
  cannot drift in observable behavior without a failing test.

## When to revisit

- pgvector becomes the right answer when (a) we ship a default
  embedder that produces vectors at append time, and (b) we have a
  user with > 100k embeddings reporting linear-scan cost.
- The advisory-lock approach should be reconsidered if we add a
  multi-master daemon topology — at that point, SERIALIZABLE
  transactions with retry budgets may be cleaner than coordinating
  advisory locks across daemons.
- `LISTEN/NOTIFY` is the right answer when multiple ctxd daemons
  share a Postgres (e.g. an HA pair behind a load balancer).
