-- 0003_caveats.sql — stateful caveat tables.
--
-- token_budgets:    per-token + currency micro-unit accumulator used
--                   by `BudgetLimit` caveat (ADR / Phase 3).
-- pending_approvals: human-in-the-loop approval queue used by the
--                   `HumanApprovalRequired` caveat.

CREATE TABLE IF NOT EXISTS token_budgets (
    token_id   TEXT NOT NULL,
    currency   TEXT NOT NULL,
    spent      BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (token_id, currency)
);

CREATE TABLE IF NOT EXISTS pending_approvals (
    approval_id  TEXT PRIMARY KEY,
    token_id     TEXT NOT NULL,
    operation    TEXT NOT NULL,
    subject      TEXT NOT NULL,
    decision     TEXT NOT NULL DEFAULT 'pending',
    requested_at TIMESTAMPTZ NOT NULL,
    decided_at   TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_approvals_decision ON pending_approvals (decision);

-- rate_buckets: per-token sliding 1-second window counter used by the
-- `rate_limit_ops_per_sec` caveat. One row per token; the upsert in
-- `PostgresCaveatState::rate_check` overwrites `(window_start, count)`
-- on every roll over. The single-row-per-token shape is intentional —
-- we only need the *current* second to admit/deny, and persisting the
-- whole history would cost more than it tells us. (See ADR 011.)
CREATE TABLE IF NOT EXISTS rate_buckets (
    token_id     TEXT PRIMARY KEY,
    window_start TIMESTAMPTZ NOT NULL,
    count        INTEGER NOT NULL DEFAULT 0
);
