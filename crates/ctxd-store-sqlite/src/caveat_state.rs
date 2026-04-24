//! SQLite-backed [`CaveatState`] implementation.
//!
//! Uses the `token_budgets` and `pending_approvals` tables created in
//! v0.3 schema initialization (see `store::initialize`).

use async_trait::async_trait;
use ctxd_cap::state::{ApprovalDecision, CaveatState};
use ctxd_cap::CapError;

use crate::store::EventStore;

/// A thin wrapper that exposes an [`EventStore`]'s
/// `token_budgets`/`pending_approvals` tables as a [`CaveatState`] impl.
///
/// Kept as a wrapper (rather than implementing `CaveatState` directly
/// on `EventStore`) so downstream crates can depend on either the
/// stateless store surface or the stateful one without an orphan-rule
/// headache.
#[derive(Clone, Debug)]
pub struct SqliteCaveatState {
    store: EventStore,
}

impl SqliteCaveatState {
    /// Wrap a store so it can back stateful caveats.
    pub fn new(store: EventStore) -> Self {
        Self { store }
    }
}

fn map_err(e: sqlx::Error) -> CapError {
    CapError::Denied(format!("caveat state db error: {e}"))
}

#[async_trait]
impl CaveatState for SqliteCaveatState {
    async fn budget_increment(
        &self,
        token_id: &str,
        currency: &str,
        amount_micro_units: i64,
    ) -> Result<i64, CapError> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.store.pool().begin().await.map_err(map_err)?;
        // Upsert-and-return: SQLite doesn't support RETURNING in old
        // versions, but sqlx + modern SQLite do. We use two statements
        // inside a transaction to stay portable.
        sqlx::query(
            r#"
            INSERT INTO token_budgets (token_id, currency, spent, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(token_id, currency) DO UPDATE SET
                spent = spent + excluded.spent,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(token_id)
        .bind(currency)
        .bind(amount_micro_units)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        let row: (i64,) =
            sqlx::query_as("SELECT spent FROM token_budgets WHERE token_id = ? AND currency = ?")
                .bind(token_id)
                .bind(currency)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(row.0)
    }

    async fn budget_get(&self, token_id: &str, currency: &str) -> Result<i64, CapError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT spent FROM token_budgets WHERE token_id = ? AND currency = ?")
                .bind(token_id)
                .bind(currency)
                .fetch_optional(self.store.pool())
                .await
                .map_err(map_err)?;
        Ok(row.map(|(v,)| v).unwrap_or(0))
    }

    async fn rate_check(
        &self,
        _token_id: &str,
        _op: &str,
        _rate_ops_per_sec: u32,
    ) -> Result<bool, CapError> {
        // SQLite-backed rate limiting is deferred — the in-memory fast
        // path in `ctxd-cli::rate_limit::RateLimiter` is the hot path.
        // We intentionally return `true` here so call sites that layer
        // both remain correct under either backend.
        Ok(true)
    }

    async fn approval_request(
        &self,
        approval_id: &str,
        token_id: &str,
        operation: &str,
        subject: &str,
    ) -> Result<(), CapError> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO pending_approvals (approval_id, token_id, operation, subject, decision, requested_at)
            VALUES (?, ?, ?, ?, 'pending', ?)
            ON CONFLICT(approval_id) DO NOTHING
            "#,
        )
        .bind(approval_id)
        .bind(token_id)
        .bind(operation)
        .bind(subject)
        .bind(&now)
        .execute(self.store.pool())
        .await
        .map_err(map_err)?;
        Ok(())
    }

    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalDecision, CapError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT decision FROM pending_approvals WHERE approval_id = ?")
                .bind(approval_id)
                .fetch_optional(self.store.pool())
                .await
                .map_err(map_err)?;
        match row.map(|(s,)| s).as_deref() {
            Some("pending") => Ok(ApprovalDecision::Pending),
            Some("allow") => Ok(ApprovalDecision::Allow),
            Some("deny") => Ok(ApprovalDecision::Deny),
            Some(other) => Err(CapError::Denied(format!(
                "unknown approval decision: {other}"
            ))),
            None => Err(CapError::Denied(format!("no such approval: {approval_id}"))),
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
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE pending_approvals
            SET decision = ?, decided_at = ?
            WHERE approval_id = ?
            "#,
        )
        .bind(decision_str)
        .bind(&now)
        .bind(approval_id)
        .execute(self.store.pool())
        .await
        .map_err(map_err)?;
        if result.rows_affected() == 0 {
            return Err(CapError::Denied(format!("no such approval: {approval_id}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_budget_increment_persists() {
        let store = EventStore::open_memory().await.unwrap();
        let st = SqliteCaveatState::new(store);
        let t1 = st.budget_increment("tok-1", "USD", 500).await.unwrap();
        assert_eq!(t1, 500);
        let t2 = st.budget_increment("tok-1", "USD", 250).await.unwrap();
        assert_eq!(t2, 750);
        let got = st.budget_get("tok-1", "USD").await.unwrap();
        assert_eq!(got, 750);
    }

    #[tokio::test]
    async fn sqlite_approval_roundtrip() {
        let store = EventStore::open_memory().await.unwrap();
        let st = SqliteCaveatState::new(store);
        st.approval_request("a1", "tok-1", "write", "/x")
            .await
            .unwrap();
        assert_eq!(
            st.approval_status("a1").await.unwrap(),
            ApprovalDecision::Pending
        );
        st.approval_decide("a1", ApprovalDecision::Allow)
            .await
            .unwrap();
        assert_eq!(
            st.approval_status("a1").await.unwrap(),
            ApprovalDecision::Allow
        );
    }

    #[tokio::test]
    async fn sqlite_approval_missing_is_err() {
        let store = EventStore::open_memory().await.unwrap();
        let st = SqliteCaveatState::new(store);
        assert!(st.approval_status("missing").await.is_err());
        assert!(st
            .approval_decide("missing", ApprovalDecision::Allow)
            .await
            .is_err());
    }
}
