# ADR 006 — Federation conflict resolution: LWW with UUIDv7 tiebreak

## Status

Accepted, v0.3.

## Context

Under federation, two peers can independently append events to the same
subject. The canonical event log keeps both branches forever (parents
from different peers diverge; hash chains stay intact per-peer). The
question is: what does a materialized view (KV, entity, timeline)
return when two events claim to be the latest value?

We need a deterministic resolution rule that every peer converges on,
without requiring a coordinator.

## Decision

The **KV materialized view uses last-writer-wins (LWW) on `(time,
event_id)`** with the following precedence:

1. Higher `time` wins.
2. On equal `time`, the lexicographically-greater `event_id` wins.
3. Because `event_id` is UUIDv7, the lexicographic ordering is also
   monotonic in the ms-resolution embedded timestamp, so ties are
   broken by a byte-stable random tail.

This is deterministic across peers: each peer sees the same
`(time, id)` tuples and derives the same winner.

The **event log itself is not modified** — both branches remain
addressable via `ctx_read` + `ctx_timeline`. Only derived views apply
LWW. `read` for a subject returns both branches in `(time, id)` order,
so a caller can observe the divergence.

## Rationale

- **UUIDv7 is already our id format**, so the tiebreak comes for free.
- **No Lamport clocks / vector clocks** — those would need agreement
  on peer membership, which we don't want to require.
- **Physical time only** — ctxd runs on machines where NTP is reliable
  enough; we accept that two peers with 50ms clock skew can have a 50ms
  window of reordering. Callers who need stricter consistency should
  use `read` + `parents` to see the full DAG.
- **Parents still carry causal truth** — the log preserves the actual
  DAG; LWW is a projection for KV only.

## Consequences

- `kv_get` is deterministic under federation.
- `ctx_timeline` is deterministic under federation (it returns log
  events in `(time, seq)` order; under replication, `seq` is local but
  `time` is shared, so ordering within a subject is stable).
- The `read` path is explicitly the source of truth when a caller
  needs to see concurrent branches — this is called out in
  `docs/federation.md` (to be written as part of 2H).

## Revisit

If peer clocks drift by >1s routinely (e.g. peers on mobile devices
behind lossy NTP), we should revisit and add a "wait for N seconds
before LWW takes effect" stabilization window.
