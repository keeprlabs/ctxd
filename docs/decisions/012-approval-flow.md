# ADR 012 — Human-approval flow

**Status:** Accepted (v0.3 Phase 3)
**Date:** 2026-04-24
**Related:** ADR 011 (caveat-state wiring), ADR 002 (capability verification)

## Context

The `HumanApprovalRequired(op)` caveat says: before the verifier
returns Ok for `op`, a human must explicitly approve. Use cases:

- Production write paths that an autonomous agent attempts.
- Cross-tenant searches that surface privileged data.
- Federation peer admissions on a strict policy.

The caveat needs:

1. A way for the verifier to **request** an approval and block on it.
2. A way for a human to **decide** the approval (CLI, HTTP, future
   notifier integrations).
3. A way for the verifier task to **wake up** when the decision lands,
   without busy-polling.
4. A **timeout** so a stuck verify can't hold a request handler open
   forever.

## Decision

### 1. Three trait methods on `CaveatState`

```rust
async fn approval_request(approval_id, token_id, operation, subject) -> Result<(), CapError>;
async fn approval_status(approval_id) -> Result<ApprovalDecision, CapError>;
async fn approval_decide(approval_id, decision) -> Result<(), CapError>;
async fn approval_wait(approval_id, timeout) -> Result<ApprovalDecision, CapError>;
```

- `approval_request` is idempotent on `(approval_id)` — repeated calls
  with the same id never demote an already-decided row to Pending.
- `approval_decide` enforces "no double-decide": the second call on
  an already-decided row returns `CapError::Denied`. This is the
  defense against an attacker who steals an approval id and tries to
  flip a `Deny` to an `Allow` after the fact.
- `approval_wait` is the wait-and-resume primitive. The default impl
  polls with exponential backoff (25 ms → 400 ms cap). Concrete impls
  override with a Notify-backed fast path so multiple concurrent
  waiters all wake on a single decide.

### 2. Notify-based fast path on the in-memory impl

`InMemoryCaveatState` keeps a `HashMap<approval_id, Arc<Notify>>`. On
`approval_decide` it calls `notify_waiters` for the matching id; the
parked waiter task races the wake against its own `tokio::time::sleep`
and re-checks status on either fire. Multiple waiters all unpark.

The SQLite impl uses the default polling implementation. This is fine
for v0.3 — pending approvals are rare (per-op-per-token), and we'd
rather lean on a proven path than spin up a Postgres-`LISTEN`-style
notifier on top of SQLite. When a Postgres backend lands the trait
method gives each backend the freedom to override.

### 3. Decision sources

- **CLI**: `ctxd approve --id <uuid> --decision allow|deny` opens the
  daemon database directly. Works even when `ctxd serve` is down —
  ops scenario for emergency denies.
- **HTTP**: `POST /v1/approvals/:id/decide` body `{ "decision":
  "allow" | "deny", "token": "<base64>"? }`. Optional admin token
  (verified with `Operation::Admin`). Returns 200 + JSON on success,
  400 on bad payload, 409 on double-decide.
- **GET /v1/approvals**: lists pending rows. Best-effort enumeration
  via the SQLite store — the trait deliberately doesn't carry a
  `list` method (we don't want to commit to an enumerate API on
  Postgres et al. before we have a real use case).

### 4. Notifier broadcast channel

The daemon constructs a `tokio::sync::broadcast::Sender<PendingApproval>`
at startup and holds it alive for the duration of `serve`. Future
notifier adapters (Slack, email, push) `subscribe()` and forward
events out-of-band. **No notifier adapter ships in v0.3** — this is
just plumbing.

The verifier doesn't push to this channel itself today; the channel
is the integration point for a future notifier service.

### 5. Timeout, no auto-retry

Each verify creates a fresh approval. If a token is presented twice
for the same `(token_id, op, subject)`, two approvals are created.
This is intentional:

- An approval is not a license to repeat the operation. It's a license
  for *this verify call*.
- Re-using a prior approval would let an attacker who once got an
  approval keep replaying the operation indefinitely.

Timeouts default to 5 minutes (the MCP server's `DEFAULT_APPROVAL_TIMEOUT`).
Each transport can override. On timeout the verifier returns
`CapError::ApprovalTimeout` — the row stays `pending` so a late
decision is still recorded for audit, but the verify call bailed.

### 6. Double-decide is a hard error

```sql
UPDATE pending_approvals
SET decision = ?, decided_at = ?
WHERE approval_id = ? AND decision = 'pending'
```

The `decision = 'pending'` predicate is the concurrency guard. Two
concurrent decides race for `rows_affected = 1`; the loser sees
`rows_affected = 0` and returns `CapError::Denied("race-lost")`.
Both impls also reject decisions back to `Pending` (`approval_decide(_,
ApprovalDecision::Pending) → Err`).

## Alternatives considered

- **Approval reuse**: keep one approval per `(token_id, op, subject)`
  and short-circuit subsequent verifies. Rejected — gives the
  attacker a one-shot-many-uses primitive.
- **Polling-only `approval_wait` with no Notify**: simpler but burns
  10 ms of CPU per pending approval. The Notify-backed in-memory
  path is a small amount of code and matches the test harness's
  expected snappiness.
- **Putting the broadcast channel on the trait**: rejected as
  overfit. The channel is a *transport* concern; not every CaveatState
  backend wants to publish to it. The daemon owns the sender and
  routes to whichever adapters are wired.

## Conditions that would make us revisit

- A real notifier adapter ships and discovers the broadcast carrier
  is missing fields → extend `PendingApproval`.
- We add a Postgres backend → swap polling for a `LISTEN/NOTIFY`-backed
  override of `approval_wait` per backend.
- An operator scenario emerges where one approval *should* gate
  multiple verifies (e.g. a batch of 100 writes) → introduce a
  separate `BatchApprovalRequired` caveat rather than weaken the
  per-verify rule.
