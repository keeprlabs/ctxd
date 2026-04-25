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
