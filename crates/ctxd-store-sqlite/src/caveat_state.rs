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

    async fn rate_check(&self, token_id: &str, ops_per_sec: u32) -> Result<bool, CapError> {
        // Sliding 1-second window counter persisted in `rate_buckets`.
        // The PRIMARY KEY on `token_id` makes the upsert atomic per
        // SQLite's per-table write lock. On a different second we
        // *replace* `(window_start, count)` with `(now_secs, 1)`; on
        // the same second we increment.
        //
        // We do this in a single statement using `INSERT … ON CONFLICT`
        // so two concurrent verifies for the same token cannot both
        // observe a pre-increment count and both succeed.
        let now_secs = chrono::Utc::now().timestamp();
        let mut tx = self.store.pool().begin().await.map_err(map_err)?;
        sqlx::query(
            r#"
            INSERT INTO rate_buckets (token_id, window_start, count)
            VALUES (?, ?, 1)
            ON CONFLICT(token_id) DO UPDATE SET
                count = CASE
                    WHEN rate_buckets.window_start = excluded.window_start THEN rate_buckets.count + 1
                    ELSE 1
                END,
                window_start = excluded.window_start
            "#,
        )
        .bind(token_id)
        .bind(now_secs)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        let row: (i64, i64) = sqlx::query_as(
            "SELECT window_start, count FROM rate_buckets WHERE token_id = ?",
        )
        .bind(token_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        // The post-increment count is what we compare. The ELSE arm
        // above has already reset count to 1 for a new window so this
        // comparison is always against the current window's hits.
        debug_assert_eq!(row.0, now_secs, "rate window should have rolled to now");
        Ok(row.1 <= ops_per_sec as i64)
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
        if decision == ApprovalDecision::Pending {
            return Err(CapError::Denied(
                "cannot decide an approval back to Pending".to_string(),
            ));
        }
        let decision_str = match decision {
            ApprovalDecision::Allow => "allow",
            ApprovalDecision::Deny => "deny",
            ApprovalDecision::Pending => unreachable!("guarded above"),
        };
        let now = chrono::Utc::now().to_rfc3339();

        // Pre-check the row's existence + state. We need three distinct
        // outcomes (no-such-row, already-decided, success) so a single
        // `UPDATE … WHERE decision='pending'` can't tell them apart on
        // its own.
        let row: Option<(String,)> =
            sqlx::query_as("SELECT decision FROM pending_approvals WHERE approval_id = ?")
                .bind(approval_id)
                .fetch_optional(self.store.pool())
                .await
                .map_err(map_err)?;
        let prior = match row {
            None => return Err(CapError::Denied(format!("no such approval: {approval_id}"))),
            Some((s,)) => s,
        };
        if prior != "pending" {
            return Err(CapError::Denied(format!(
                "approval {approval_id} already decided as {prior}"
            )));
        }

        // Conditional UPDATE. The `decision = 'pending'` predicate is
        // the actual concurrency guard — if two `approval_decide` calls
        // race, only the first lands rows_affected = 1 and the second
        // sees rows_affected = 0 and falls into the `already decided`
        // error path below.
        let result = sqlx::query(
            r#"
            UPDATE pending_approvals
            SET decision = ?, decided_at = ?
            WHERE approval_id = ? AND decision = 'pending'
            "#,
        )
        .bind(decision_str)
        .bind(&now)
        .bind(approval_id)
        .execute(self.store.pool())
        .await
        .map_err(map_err)?;
        if result.rows_affected() == 0 {
            return Err(CapError::Denied(format!(
                "approval {approval_id} race-lost: already decided"
            )));
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

    #[tokio::test]
    async fn sqlite_approval_double_decide_rejected() {
        let store = EventStore::open_memory().await.unwrap();
        let st = SqliteCaveatState::new(store);
        st.approval_request("a1", "tok-1", "write", "/x")
            .await
            .unwrap();
        st.approval_decide("a1", ApprovalDecision::Allow)
            .await
            .unwrap();
        // Second decide must fail — guarantees that an attacker who
        // grabs an approval id can't flip Deny → Allow after the fact.
        let res = st.approval_decide("a1", ApprovalDecision::Deny).await;
        assert!(res.is_err());
        // Status remains Allow.
        assert_eq!(
            st.approval_status("a1").await.unwrap(),
            ApprovalDecision::Allow
        );
    }

    #[tokio::test]
    async fn sqlite_approval_decide_rejects_pending_decision() {
        let store = EventStore::open_memory().await.unwrap();
        let st = SqliteCaveatState::new(store);
        st.approval_request("a1", "tok-1", "write", "/x")
            .await
            .unwrap();
        let res = st.approval_decide("a1", ApprovalDecision::Pending).await;
        assert!(res.is_err());
    }
}
