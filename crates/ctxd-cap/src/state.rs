//! Stateful caveat backing store.
//!
//! Some biscuit caveats evaluate against persistent state —
//! [`BudgetLimit`] tracks accumulated spend per token + currency, and
//! `HumanApprovalRequired` blocks until a human decides via
//! `ctxd approve`. Both need storage outside the token itself.
//!
//! The [`CaveatState`] trait isolates that storage behind an
//! async interface so both the default SQLite-backed daemon and an
//! in-memory test harness can plug in.
//!
//! v0.3 scope: the trait and an in-memory reference implementation are
//! shipped here. The SQLite-backed impl lives in `ctxd-store-sqlite`
//! under a `CaveatState` impl for `EventStore` (Phase 3A follow-up).
//! `HumanApprovalRequired` wiring into `verify` plus the `ctxd approve`
//! CLI command are deferred to the Phase 3 follow-up PR; see
//! `docs/plans/v0.3-progress.md`.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::CapError;

/// A human approval decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// The approval is outstanding — no decision yet.
    Pending,
    /// The human explicitly allowed the operation.
    Allow,
    /// The human explicitly denied the operation.
    Deny,
}

/// State backing for stateful caveats.
///
/// Every method is async-capable so backends can be SQLite, Postgres, or
/// a remote service. The default in-memory impl is suitable for tests
/// and single-process daemons with short budgets.
#[async_trait]
pub trait CaveatState: Send + Sync {
    /// Charge `amount_micro_units` against a `(token_id, currency)`
    /// budget and return the new total. The caller checks the returned
    /// total against the caveat's declared limit.
    async fn budget_increment(
        &self,
        token_id: &str,
        currency: &str,
        amount_micro_units: i64,
    ) -> Result<i64, CapError>;

    /// Current budget total for a `(token_id, currency)` pair.
    async fn budget_get(&self, token_id: &str, currency: &str) -> Result<i64, CapError>;

    /// Check whether an operation on `(token_id, op, subject)` is
    /// within rate limits. Returns `Ok(true)` when the op can proceed,
    /// `Ok(false)` when it must be rejected.
    ///
    /// Default impl in memory is always `true`; concrete backends
    /// implement sliding-window counters.
    async fn rate_check(
        &self,
        token_id: &str,
        op: &str,
        rate_ops_per_sec: u32,
    ) -> Result<bool, CapError>;

    /// Record a pending approval request. Returns the approval id
    /// (caller typically uses a UUIDv7 they already generated).
    async fn approval_request(
        &self,
        approval_id: &str,
        token_id: &str,
        operation: &str,
        subject: &str,
    ) -> Result<(), CapError>;

    /// Fetch the current decision for an approval. Returns
    /// [`ApprovalDecision::Pending`] if the request exists but hasn't
    /// been decided; returns [`CapError::Denied`] if no such request.
    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalDecision, CapError>;

    /// Record an approval decision (called by `ctxd approve`).
    async fn approval_decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), CapError>;
}

/// In-memory backing for tests and simple single-process daemons.
///
/// This is deliberately simple: no rate limiting, no persistence, no
/// cross-process coordination. It exists so unit tests can exercise
/// the trait contract without spinning up a real store.
#[derive(Debug, Default)]
pub struct InMemoryCaveatState {
    budgets: Mutex<HashMap<(String, String), i64>>,
    approvals: Mutex<HashMap<String, ApprovalDecision>>,
}

impl InMemoryCaveatState {
    /// Create a fresh in-memory backing.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CaveatState for InMemoryCaveatState {
    async fn budget_increment(
        &self,
        token_id: &str,
        currency: &str,
        amount_micro_units: i64,
    ) -> Result<i64, CapError> {
        let mut g = self
            .budgets
            .lock()
            .map_err(|_| CapError::Denied("budget lock poisoned".to_string()))?;
        let key = (token_id.to_string(), currency.to_string());
        let entry = g.entry(key).or_insert(0);
        *entry = entry.saturating_add(amount_micro_units);
        Ok(*entry)
    }

    async fn budget_get(&self, token_id: &str, currency: &str) -> Result<i64, CapError> {
        let g = self
            .budgets
            .lock()
            .map_err(|_| CapError::Denied("budget lock poisoned".to_string()))?;
        Ok(g.get(&(token_id.to_string(), currency.to_string()))
            .copied()
            .unwrap_or(0))
    }

    async fn rate_check(
        &self,
        _token_id: &str,
        _op: &str,
        _rate_ops_per_sec: u32,
    ) -> Result<bool, CapError> {
        Ok(true)
    }

    async fn approval_request(
        &self,
        approval_id: &str,
        _token_id: &str,
        _operation: &str,
        _subject: &str,
    ) -> Result<(), CapError> {
        let mut g = self
            .approvals
            .lock()
            .map_err(|_| CapError::Denied("approvals lock poisoned".to_string()))?;
        g.entry(approval_id.to_string())
            .or_insert(ApprovalDecision::Pending);
        Ok(())
    }

    async fn approval_status(&self, approval_id: &str) -> Result<ApprovalDecision, CapError> {
        let g = self
            .approvals
            .lock()
            .map_err(|_| CapError::Denied("approvals lock poisoned".to_string()))?;
        g.get(approval_id)
            .copied()
            .ok_or_else(|| CapError::Denied(format!("no such approval: {approval_id}")))
    }

    async fn approval_decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), CapError> {
        let mut g = self
            .approvals
            .lock()
            .map_err(|_| CapError::Denied("approvals lock poisoned".to_string()))?;
        g.insert(approval_id.to_string(), decision);
        Ok(())
    }
}

/// A budget limit expressed in micro-units of a currency.
///
/// Micro-units avoid floating-point: `$0.42` is `420_000`
/// micro-USD. Operations subtract from this cap via
/// [`CaveatState::budget_increment`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetLimit {
    /// Free-form currency code, e.g. `"USD"` or `"OPENAI_TOKENS"`.
    pub currency: String,
    /// Hard cap in micro-units of the currency.
    pub amount_micro_units: i64,
}

impl BudgetLimit {
    /// Convenience: check that a proposed increment would stay under
    /// the budget.
    pub fn check(&self, total_after_increment: i64) -> Result<(), CapError> {
        if total_after_increment > self.amount_micro_units {
            Err(CapError::Denied(format!(
                "budget exceeded for {}: {} > {}",
                self.currency, total_after_increment, self.amount_micro_units
            )))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn budget_increments_monotonically() {
        let st = InMemoryCaveatState::new();
        let total = st.budget_increment("tok-1", "USD", 100).await.unwrap();
        assert_eq!(total, 100);
        let total = st.budget_increment("tok-1", "USD", 250).await.unwrap();
        assert_eq!(total, 350);
        let current = st.budget_get("tok-1", "USD").await.unwrap();
        assert_eq!(current, 350);

        // Separate currencies do not interfere.
        let other = st.budget_get("tok-1", "OPENAI_TOKENS").await.unwrap();
        assert_eq!(other, 0);
    }

    #[tokio::test]
    async fn budget_limit_check_rejects_overage() {
        let limit = BudgetLimit {
            currency: "USD".to_string(),
            amount_micro_units: 1_000_000,
        };
        assert!(limit.check(999_999).is_ok());
        assert!(limit.check(1_000_000).is_ok());
        assert!(limit.check(1_000_001).is_err());
    }

    #[tokio::test]
    async fn approval_lifecycle() {
        let st = InMemoryCaveatState::new();
        st.approval_request("appr-1", "tok-1", "write", "/work/x")
            .await
            .unwrap();
        assert_eq!(
            st.approval_status("appr-1").await.unwrap(),
            ApprovalDecision::Pending
        );
        st.approval_decide("appr-1", ApprovalDecision::Allow)
            .await
            .unwrap();
        assert_eq!(
            st.approval_status("appr-1").await.unwrap(),
            ApprovalDecision::Allow
        );
        // Unknown approval is an error.
        assert!(st.approval_status("nope").await.is_err());
    }
}
