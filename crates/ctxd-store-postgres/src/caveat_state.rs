//! Postgres-backed [`CaveatState`] implementation.
//!
//! Mirrors `ctxd-store-sqlite::caveat_state::SqliteCaveatState` so a
//! daemon can swap backends with no behavior change.

use async_trait::async_trait;
use ctxd_cap::state::{ApprovalDecision, CaveatState};
use ctxd_cap::CapError;
use sqlx::Row;

use crate::store::PostgresStore;

/// Wrap a [`PostgresStore`] so it can back stateful caveats.
#[derive(Clone, Debug)]
pub struct PostgresCaveatState {
    store: PostgresStore,
}

impl PostgresCaveatState {
    /// Build a new `PostgresCaveatState` over the supplied store.
    pub fn new(store: PostgresStore) -> Self {
        Self { store }
    }
}

fn map_err(e: sqlx::Error) -> CapError {
    CapError::Denied(format!("caveat state db error: {e}"))
}

#[async_trait]
impl CaveatState for PostgresCaveatState {
    async fn budget_increment(
        &self,
        token_id: &str,
        currency: &str,
        amount_micro_units: i64,
    ) -> Result<i64, CapError> {
        // Single statement with RETURNING — atomic and faster than the
        // SQLite two-step (which exists only because older SQLite
        // versions lack RETURNING).
        let row = sqlx::query(
            r#"
            INSERT INTO token_budgets (token_id, currency, spent, updated_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (token_id, currency) DO UPDATE SET
                spent      = token_budgets.spent + EXCLUDED.spent,
                updated_at = EXCLUDED.updated_at
            RETURNING spent
            "#,
        )
        .bind(token_id)
        .bind(currency)
        .bind(amount_micro_units)
        .bind(chrono::Utc::now())
        .fetch_one(self.store.pool())
        .await
        .map_err(map_err)?;
        let spent: i64 = row
            .try_get("spent")
            .map_err(|e| CapError::Denied(format!("budget RETURNING decode: {e}")))?;
        Ok(spent)
    }

    async fn budget_get(&self, token_id: &str, currency: &str) -> Result<i64, CapError> {
        let row = sqlx::query(
            "SELECT spent FROM token_budgets WHERE token_id = $1 AND currency = $2",
        )
        .bind(token_id)
        .bind(currency)
        .fetch_optional(self.store.pool())
        .await
        .map_err(map_err)?;
        match row {
            Some(r) => {
                let spent: i64 = r
                    .try_get("spent")
                    .map_err(|e| CapError::Denied(format!("budget SELECT decode: {e}")))?;
                Ok(spent)
            }
            None => Ok(0),
        }
    }

    async fn rate_check(
        &self,
        _token_id: &str,
        _op: &str,
        _rate_ops_per_sec: u32,
    ) -> Result<bool, CapError> {
        // Same policy as SQLite — the in-memory rate limiter is the
        // hot path; persistent rate limiting is deferred.
        Ok(true)
    }

    async fn approval_request(
        &self,
        approval_id: &str,
        token_id: &str,
        operation: &str,
        subject: &str,
    ) -> Result<(), CapError> {
        sqlx::query(
            r#"
            INSERT INTO pending_approvals (approval_id, token_id, operation, subject, decision, requested_at)
            VALUES ($1, $2, $3, $4, 'pending', $5)
            ON CONFLICT (approval_id) DO NOTHING
            "#,
        )
        .bind(approval_id)
        .bind(token_id)
        .bind(operation)
        .bind(subject)
        .bind(chrono::Utc::now())
        .execute(self.store.pool())
        .await
        .map_err(map_err)?;
        Ok(())
    }

    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalDecision, CapError> {
        let row = sqlx::query(
            "SELECT decision FROM pending_approvals WHERE approval_id = $1",
        )
        .bind(approval_id)
        .fetch_optional(self.store.pool())
        .await
        .map_err(map_err)?;
        match row {
            Some(r) => {
                let decision: String = r
                    .try_get("decision")
                    .map_err(|e| CapError::Denied(format!("approval decision decode: {e}")))?;
                match decision.as_str() {
                    "pending" => Ok(ApprovalDecision::Pending),
                    "allow" => Ok(ApprovalDecision::Allow),
                    "deny" => Ok(ApprovalDecision::Deny),
                    other => Err(CapError::Denied(format!(
                        "unknown approval decision: {other}"
                    ))),
                }
            }
            None => Err(CapError::Denied(format!(
                "no such approval: {approval_id}"
            ))),
        }
    }

    async fn approval_decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), CapError> {
        let decision_str = match decision {
            ApprovalDecision::Pending => "pending",
            ApprovalDecision::Allow => "allow",
            ApprovalDecision::Deny => "deny",
        };
        let result = sqlx::query(
            r#"
            UPDATE pending_approvals
            SET decision = $1, decided_at = $2
            WHERE approval_id = $3
            "#,
        )
        .bind(decision_str)
        .bind(chrono::Utc::now())
        .bind(approval_id)
        .execute(self.store.pool())
        .await
        .map_err(map_err)?;
        if result.rows_affected() == 0 {
            return Err(CapError::Denied(format!(
                "no such approval: {approval_id}"
            )));
        }
        Ok(())
    }
}
