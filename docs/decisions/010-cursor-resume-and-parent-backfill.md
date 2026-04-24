# ADR 010 — Cursor resume and parent backfill

## Status

Accepted, v0.3 Phase 2E.

## Context

Two failure modes break naïve federation:

1. **Crash during replication.** Sender pushes events 1..N, dies after
   sending N/2 with no record of how far it got. Restart loses
   everything not yet ACKed.
2. **Out-of-order parents.** A receives a child event whose `parents`
   reference event ids it doesn't have (the parent took a different
   replication path or was committed during a network partition).
   Without help, the child would either be rejected or stored with
   broken ancestry.

We need a resume mechanism that's safe across restarts and a way to
fill in missing parents when they arrive after the child.

## Decision

### Cursor resume

Each peer maintains a **receive-cursor** in `peer_cursors` keyed by
`(peer_id, subject_pattern)`:

| field             | meaning                                              |
| ----------------- | ---------------------------------------------------- |
| `last_event_id`   | UUIDv7 of the most recent event we've received       |
| `last_event_time` | RFC-3339 timestamp of the most recent event          |
| `updated_at`      | wall-clock of the cursor update                      |

The cursor is **persisted on every successful inbound ACK**, before
the response is written. Sender-side replay reads the *receiver's*
cursor:

1. Local sends `PeerCursorRequest { peer_id, subject_pattern }`.
2. Remote returns `peer_cursor_get(local_peer_id, subject_pattern)` —
   what *the remote* last saw from us.
3. Local reads its own store with `time > cursor.last_event_time`,
   filters by `subject_pattern`, and streams the result via
   `PeerReplicate`.

Worst-case behaviour after a crash: duplicate delivery. The receiver's
`Store::append` UNIQUE constraint on `events.id` makes this a no-op —
duplicates are silently absorbed and the cursor advances anyway.

### Parent backfill

When `handle_peer_replicate` sees an event with non-empty `parents`,
it computes the set of parent ids missing from the local store. If
non-empty, it sends `PeerFetchEvents { event_ids }` to the origin and
applies the returned events in **topological order** before appending
the original child.

Backfilled events still go through `verify_inbound` — signature must
verify against the origin pubkey, subject must fit the granted glob.
A backfill failure logs a warning and we proceed with the child
append: the child is still useful for views even with broken
ancestry.

The toposort is iterative: walk the pending list, append events whose
parents are already satisfied, repeat. If a pass makes no progress,
we error out rather than infinite-loop ("backfill stalled" message).

## Rationale

- **Receiver-side cursor is canonical.** Storing the cursor on the
  receiver means a sender can re-derive resume state by asking, with
  zero local persistent state in `send_replicate`. This is symmetric
  and crash-safe.
- **Idempotent replay > "exactly once".** UUID-keyed UNIQUE makes
  duplicate delivery free. Building exactly-once on top would require
  acknowledged-by-id tracking on the sender, more state, and a
  durable outbox. We don't need it.
- **Toposort over recursive fetch.** A naïve recursive
  `PeerFetchEvents` would race with concurrent appends and could
  spawn unbounded round-trips. The bounded toposort over a single
  fetched batch is simpler to reason about; if a backfill needs
  multiple rounds, the next inbound replicate will trigger the next
  round.

## Consequences

- A peer's cursor is a function of (receiver, sender, subject_pattern).
  Adding/changing subject patterns introduces a fresh cursor at zero,
  which forces a full replay on the new pattern. That's a cheap
  correctness choice — the alternative would be merging old patterns
  into new ones, which is hard to make idempotent.
- The `PeerFetchEvents` handler currently does a full-store scan to
  resolve event ids. That's O(N) per backfill; for large stores we'll
  want an event-id index. Tracked as a v0.4 perf item.
- Parent backfill sets a lower bound on hop latency: a missing parent
  costs an extra round-trip. We accept that — most events have empty
  parents and the cost only applies to merge-DAG events.

## Revisit

If federation perf benchmarks show backfill latency dominating, add
an event-id index to the SQLite schema (separate from the implicit
`UNIQUE` on `events.id` — that's a btree on the table, but adding a
`CREATE INDEX idx_events_id ON events(id)` would let us short-circuit
the recursive read).
