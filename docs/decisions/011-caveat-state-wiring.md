# ADR 011 — Caveat-state wiring on `CapEngine::verify`

**Status:** Accepted (v0.3 Phase 3)
**Date:** 2026-04-24
**Related:** ADR 002 (capability verification), ADR 004 (open-by-default), ADR 012 (approval flow)

## Context

`CapEngine::verify` ships static caveats since v0.1: subject glob,
operation set, expiry, kind. v0.2 added rate-limit and budget *facts*
on tokens but no state-backed enforcement. v0.3 needs to enforce two
new caveats that *cannot* be evaluated without persistent state:

- `BudgetLimit(currency, amount_micro_units)`: cumulative spend must stay
  under `amount`.
- `HumanApprovalRequired(op)`: a human must explicitly allow each verify
  for `op` before the verifier returns Ok.

Both demand a backing store. They also raise a wiring question — how
do existing callers of `verify` (MCP, HTTP, wire protocol) opt in
without either a flag day or a silent downgrade where the new caveat
isn't enforced?

## Decision

### 1. New entry point, old entry point preserved

We add `CapEngine::verify_with_state(token, subject, op, etype, state, approval_timeout)`
where `state: Option<&dyn CaveatState>`. The legacy
`CapEngine::verify(token, subject, op, etype)` remains and *delegates
to v0.2 semantics* — static caveats are enforced, stateful caveats are
observed-but-not-enforced.

This means:

- A pre-v0.3 caller (or a test that doesn't care about budgets) keeps
  working unchanged.
- A new caller that wants enforcement explicitly threads an
  `Arc<dyn CaveatState>` and calls `verify_with_state`.

The daemon (`ctxd serve`) constructs `SqliteCaveatState::new(store)`
once at startup and shares the `Arc<dyn CaveatState>` with the HTTP
router, the MCP server, and any other transport that calls
`verify_with_state`.

### 2. Fallback when `state = None`

Two cases:

- **Budget**: returns `Ok` (caveat observed, not enforced). Same as
  v0.2 semantics. Documented contract.
- **Approval**: returns `CapError::ApprovalStateMissing`. We refuse to
  silently downgrade an approval requirement — the entire point of
  the caveat is "no human, no go". Surfacing the error makes the
  wiring bug obvious instead of a security regression.

### 3. Operation cost table (single source of truth)

`OperationCost(u64)` is defined in `ctxd-cap/src/state.rs` with one
constant per `Operation`:

| Operation     | Cost (μUSD) | Rationale                                         |
|---------------|-------------|---------------------------------------------------|
| `read`        | 0           | Cheap point-read; budgets target writes/searches. |
| `subjects`    | 0           | Schema introspection; should not be metered.      |
| `write`       | 1_000       | Persistent state mutation: 0.001 USD.             |
| `search`      | 1_000       | Indexed lookup; same baseline as a write.         |
| `entities`    | 500         | Materialized graph read.                          |
| `related`     | 500         | Edge traversal.                                   |
| `timeline`    | 2_000       | Temporal scan; the most expensive read.           |
| `admin`       | 0           | Mint/revoke; budget isn't the right gate here.    |
| `peer`        | 0           | Federation handshake; budget is per-event.        |
| `subscribe`   | 0           | Streaming; charged via the underlying reads.      |

A regression test (`tests/budget_cost_table.rs`) freezes the
invariants: read/subjects free, write/search ≥ 1_000, timeline > write.

### 4. Reserve-then-commit, no automatic refund

Budget enforcement increments *before* the caller's op runs. If the
op fails afterwards, the budget is "spent". This is over-conservative
but gives us:

- A single round-trip to the state store per verify.
- TOCTOU-free arithmetic: with `INSERT … ON CONFLICT DO UPDATE` the
  read and increment are atomic. If we tried "read then check then
  increment" a concurrent verify could race past the cap.
- A simple mental model — verify == reserve, period.

Refund is on the v0.4 backlog as a `CaveatState::budget_refund` method.
Until then, callers who want exact accounting should not use budgets
on operations that frequently fail downstream.

### 5. Tracing, never tokens

Every approval request and decide emits a `tracing::info!` with
structured fields (`token_id`, `approval_id`, `operation`, `subject`,
`decision`). The token bytes themselves are never logged — neither
the base64 form nor the raw biscuit — to avoid token theft via log
ingestion.

## Alternatives considered

- **Hard breaking change to `verify`'s signature**: forces every call
  site to update at once. Rejected because v0.2 callers without state
  are still legitimate (e.g. attenuate-and-store flows). Keeping a
  v0.2-compatible entry point is a net win.
- **Silent budget enforcement with a default `InMemoryCaveatState`
  inside `CapEngine`**: rejected because it would mean budgets are
  reset on every `CapEngine::new()`, which silently defeats the
  caveat. Better to require the caller to supply a `state`.
- **Per-op `OperationCost` carried on the token**: rejected because
  it would let token holders forge a cheap cost. Costs must be the
  verifier's prerogative.

## Conditions that would make us revisit

- A user reports billing errors caused by failed-op-still-charged
  semantics: implement `budget_refund`.
- A new operation (e.g. `embed`) needs a cost above `timeline`:
  bump the constant and update the regression test.
- A second store backend (Postgres) lands: re-confirm the
  atomicity contract on `budget_increment` (it must use
  `UPDATE … RETURNING` or a transaction; SQLite uses the latter).
