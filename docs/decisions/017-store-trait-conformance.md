# 017 — Store trait + shared conformance suite

Status: accepted (v0.3 Phase 5)
Date: 2026-04-24

## Context

ctxd v0.2 hard-coded a single SQLite-backed `EventStore`. v0.3 added a
generic [`Store`](../../crates/ctxd-store-core/src/lib.rs) trait so
multiple backends — SQLite, Postgres, DuckDB-on-object-store, eventually
managed services — can plug in behind the same interface.

The risk with multiple backends is *behavioral drift*: subtle semantic
differences in append ordering, KV LWW, FTS ranking, or temporal
queries that surface as production bugs only in the backend we
weren't testing locally. This ADR captures the pattern we use to
catch drift before it ships.

## Decision

Every backend implements `Store` and runs the shared conformance suite
[`ctxd_store_core::testsuite`](../../crates/ctxd-store-core/src/testsuite.rs)
from its own `tests/conformance.rs`. The suite is a function
`run_all<F: Fn() -> Future<Output = S>>(factory: F)` that exercises
every trait method under the canonical happy path plus an
exhaustive-by-design list of edge cases.

Adding a new trait method is a three-step change:

1. Add the method to `Store` in `crates/ctxd-store-core/src/lib.rs`.
2. Add at least one conformance test in
   `crates/ctxd-store-core/src/testsuite.rs` that pins the expected
   behavior end-to-end.
3. Wire the impl in every existing backend (SQLite + Postgres).

Step 3 fails the workspace build until every backend implements the
new method, which keeps the trait honest. There are no default
implementations in the trait — backends must make explicit choices for
each method, and the conformance suite catches it when a backend's
choice doesn't match the trait's intent.

## Backends

| Backend | Crate | Status (v0.3) |
|---|---|---|
| SQLite | `ctxd-store-sqlite` | Default; in-memory + on-disk |
| Postgres | `ctxd-store-postgres` | Shipped (Phase 5A) |
| DuckDB + object store | `ctxd-store-duckobj` | Phase 5B (parallel agent) |

## Conformance philosophy

The suite is intentionally exhaustive at the cost of test runtime.
Every backend runs ~11 tests across append, read, recursive read,
temporal read, KV, FTS, peer registration, peer cursors, token
revocation, vector upsert+search, and parents-with-attestation. New
trait methods MUST add a test; we reject PRs that add a method without
a conformance case.

Backends MAY have additional tests beyond the conformance suite that
exercise backend-specific behavior — concurrent writers, recursive
read perf, FTS ranking — but those tests live in the backend's own
crate and don't gate other backends.

## Skipping conformance when the backend isn't reachable

The Postgres conformance test is gated on `CTXD_PG_URL` so a
contributor without a local Postgres can still run `cargo test`
across the workspace. The SQLite conformance test is unconditional
(in-memory). DuckDB-on-object-store will follow the same pattern with
a local minio / azurite fallback.

## Consequences

- Backend authors have a clear contract: pass conformance and you can
  ship.
- The trait surface stays small because every method earns its place
  via a conformance test.
- Drift is impossible without a failing CI run.
- The cost is build-time coupling: changing the trait surface ripples
  through every backend crate, but that's the *point* — it forces a
  conscious choice rather than a silent default.

## When to revisit

- If a backend can't reasonably implement a method (e.g. a
  read-replica tier that can't accept writes), introduce a
  marker-trait split (`ReadStore` / `WriteStore`) rather than adding
  default impls that hide capability gaps.
- If the conformance suite runtime starts dominating CI, partition it
  into "core" and "extended" buckets, but keep "core" running on every
  backend on every PR.
