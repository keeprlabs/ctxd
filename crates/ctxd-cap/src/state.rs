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
//! v0.3 scope: trait plus an in-memory reference implementation are
//! shipped here; the SQLite-backed impl lives in `ctxd-store-sqlite`
//! under [`crate::state::CaveatState`]. The wiring of `BudgetLimit`
//! and `HumanApprovalRequired` into [`crate::CapEngine::verify_with_state`]
//! is also done in v0.3 — see ADRs 011 and 012.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::Notify;

use crate::CapError;
use crate::Operation;

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

/// A pending approval row, broadcast on [`PendingApprovalChannel`] so
/// out-of-band notifier adapters (Slack, email, push, …) can observe new
/// requests without polling the database. Carrier only — actual decisions
/// flow back through [`CaveatState::approval_decide`].
#[derive(Debug, Clone)]
pub struct PendingApproval {
    /// Unique identifier for the approval (UUIDv7).
    pub approval_id: String,
    /// The token requesting approval.
    pub token_id: String,
    /// The operation requiring approval (e.g. `"write"`).
    pub operation: String,
    /// The subject the operation targets.
    pub subject: String,
    /// When the approval was first requested (RFC3339).
    pub requested_at: String,
}

/// Per-operation cost in micro-units of currency.
///
/// A single source of truth for how each [`Operation`] consumes a
/// [`BudgetLimit`]. Costs are intentionally coarse — we want budgets to
/// reflect dollars spent, not micro-arithmetic precision.
///
/// Cost table (v0.3, micro-USD):
///
/// | Operation     | Cost (μUSD) | Rationale                                         |
/// |---------------|-------------|---------------------------------------------------|
/// | `read`        | 0           | Cheap point-read; budgets target writes/searches. |
/// | `subjects`    | 0           | Schema introspection; should not be metered.      |
/// | `write`       | 1_000       | Persistent state mutation: 0.001 USD.             |
/// | `search`      | 1_000       | Indexed lookup; same baseline as a write.         |
/// | `entities`    | 500         | Materialized graph read.                          |
/// | `related`     | 500         | Edge traversal.                                   |
/// | `timeline`    | 2_000       | Temporal scan, likely the most expensive read.    |
/// | `admin`       | 0           | Mint/revoke; budgets are not the right gate here. |
/// | `peer`        | 0           | Federation handshake; budget is per-event.        |
/// | `subscribe`   | 0           | Streaming; charged via the underlying reads.      |
///
/// See ADR 011 for the rationale and conditions that would make us
/// revisit this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationCost(pub u64);

impl OperationCost {
    /// Cost of a read operation (μUSD).
    pub const READ: Self = Self(0);
    /// Cost of a write operation (μUSD).
    pub const WRITE: Self = Self(1_000);
    /// Cost of a subjects-list operation (μUSD).
    pub const SUBJECTS: Self = Self(0);
    /// Cost of a full-text search operation (μUSD).
    pub const SEARCH: Self = Self(1_000);
    /// Cost of an entity lookup (μUSD).
    pub const ENTITIES: Self = Self(500);
    /// Cost of a relationship lookup (μUSD).
    pub const RELATED: Self = Self(500);
    /// Cost of a temporal/timeline read (μUSD).
    pub const TIMELINE: Self = Self(2_000);
    /// Cost of an admin operation (μUSD).
    pub const ADMIN: Self = Self(0);
    /// Cost of a federation peer operation (μUSD).
    pub const PEER: Self = Self(0);
    /// Cost of a subscribe operation (μUSD).
    pub const SUBSCRIBE: Self = Self(0);

    /// Cost as `i64` for the budget store (which uses signed integers).
    pub fn as_i64(self) -> i64 {
        // Costs are u64 but bounded by per-op constants well below i64::MAX.
        self.0 as i64
    }
}

impl From<Operation> for OperationCost {
    fn from(op: Operation) -> Self {
        match op {
            Operation::Read => Self::READ,
            Operation::Write => Self::WRITE,
            Operation::Subjects => Self::SUBJECTS,
            Operation::Search => Self::SEARCH,
            Operation::Admin => Self::ADMIN,
            Operation::Peer => Self::PEER,
            Operation::Subscribe => Self::SUBSCRIBE,
        }
    }
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
    ///
    /// # Atomicity
    /// Implementations must perform read-and-update atomically (single
    /// `UPDATE … RETURNING` or wrapped in a transaction) so two
    /// concurrent verifies cannot both observe the same pre-increment
    /// total and both succeed.
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
    ///
    /// Implementations must reject duplicate `approval_id` requests
    /// (`ON CONFLICT DO NOTHING` is fine — re-request of the same id
    /// is idempotent and never demotes an already-decided row).
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
    ///
    /// Implementations MUST verify the approval row exists and is still
    /// `Pending`. A second `approval_decide` call on the same id with a
    /// different decision must return [`CapError::Denied`] — there is
    /// no double-decide. This is the single defense against an attacker
    /// who has stolen an approval id and tries to flip a `Deny` to an
    /// `Allow` after the fact.
    async fn approval_decide(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), CapError>;

    /// Block until an approval has been decided or `timeout` elapses.
    ///
    /// The caller (typically the verifier inside
    /// [`crate::CapEngine::verify_with_state`]) is suspended and resumed
    /// when [`Self::approval_decide`] is invoked from another task.
    ///
    /// # Returns
    /// - `Ok(ApprovalDecision::Allow)` / `Ok(ApprovalDecision::Deny)` —
    ///   the decision was made before the timeout.
    /// - `Ok(ApprovalDecision::Pending)` — the timeout elapsed before a
    ///   decision; the caller should treat this as a hard fail
    ///   (`ApprovalTimeout`).
    /// - `Err(CapError::Denied)` — no such approval id.
    ///
    /// # Implementation note
    /// The default impl polls [`Self::approval_status`] with exponential
    /// backoff. Concrete impls (in-memory, anything with a notifier) can
    /// override this with a [`Notify`]-backed fast path so multiple
    /// waiters all wake on a single decide.
    async fn approval_wait(
        &self,
        approval_id: &str,
        timeout: Duration,
    ) -> Result<ApprovalDecision, CapError> {
        let start = std::time::Instant::now();
        // Backoff: 25ms → 50ms → 100ms → 200ms → 400ms → 400ms …
        let mut delay = Duration::from_millis(25);
        let max_delay = Duration::from_millis(400);
        loop {
            let st = self.approval_status(approval_id).await?;
            if st != ApprovalDecision::Pending {
                return Ok(st);
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Ok(ApprovalDecision::Pending);
            }
            let remaining = timeout - elapsed;
            let sleep_for = delay.min(remaining);
            tokio::time::sleep(sleep_for).await;
            delay = (delay * 2).min(max_delay);
        }
    }
}

/// In-memory backing for tests and simple single-process daemons.
///
/// This is deliberately simple: no rate limiting, no persistence, no
/// cross-process coordination. It exists so unit tests can exercise
/// the trait contract without spinning up a real store.
///
/// Approval-wait is implemented via a per-id [`Notify`] map; multiple
/// concurrent waiters on the same approval all resume on a single
/// `approval_decide`.
#[derive(Default)]
pub struct InMemoryCaveatState {
    budgets: Mutex<HashMap<(String, String), i64>>,
    approvals: Mutex<HashMap<String, ApprovalDecision>>,
    notifiers: Mutex<HashMap<String, Arc<Notify>>>,
}

impl std::fmt::Debug for InMemoryCaveatState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryCaveatState")
            .finish_non_exhaustive()
    }
}

impl InMemoryCaveatState {
    /// Create a fresh in-memory backing.
    pub fn new() -> Self {
        Self::default()
    }

    fn notifier_for(&self, approval_id: &str) -> Result<Arc<Notify>, CapError> {
        let mut g = self
            .notifiers
            .lock()
            .map_err(|_| CapError::Denied("approvals notifier lock poisoned".to_string()))?;
        Ok(g.entry(approval_id.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone())
    }

    /// Test helper: return the id of any pending approval, or `None`.
    ///
    /// Integration tests (`approval_request_then_*.rs`) drive
    /// `verify_with_state` in a spawned task and then need to fish
    /// the just-generated approval id back out — this scan is the
    /// simplest way to do it without exposing internal storage.
    /// Production code should never need this.
    pub fn test_first_pending_approval(&self) -> Option<String> {
        let g = self.approvals.lock().ok()?;
        g.iter().find_map(|(id, dec)| {
            if *dec == ApprovalDecision::Pending {
                Some(id.clone())
            } else {
                None
            }
        })
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
        if decision == ApprovalDecision::Pending {
            return Err(CapError::Denied(
                "cannot decide an approval back to Pending".to_string(),
            ));
        }
        {
            let mut g = self
                .approvals
                .lock()
                .map_err(|_| CapError::Denied("approvals lock poisoned".to_string()))?;
            match g.get(approval_id).copied() {
                None => {
                    return Err(CapError::Denied(format!("no such approval: {approval_id}")));
                }
                Some(ApprovalDecision::Pending) => {
                    g.insert(approval_id.to_string(), decision);
                }
                Some(prior) => {
                    return Err(CapError::Denied(format!(
                        "approval {approval_id} already decided as {prior:?}"
                    )));
                }
            }
        }
        // Wake any waiters. `notify_waiters` only signals tasks already
        // parked; that's fine because waiters always re-check status
        // after wake.
        if let Ok(g) = self.notifiers.lock() {
            if let Some(n) = g.get(approval_id) {
                n.notify_waiters();
            }
        }
        Ok(())
    }

    async fn approval_wait(
        &self,
        approval_id: &str,
        timeout: Duration,
    ) -> Result<ApprovalDecision, CapError> {
        let start = std::time::Instant::now();
        let notifier = self.notifier_for(approval_id)?;
        loop {
            let st = self.approval_status(approval_id).await?;
            if st != ApprovalDecision::Pending {
                return Ok(st);
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Ok(ApprovalDecision::Pending);
            }
            let remaining = timeout - elapsed;
            // Park on the notifier OR the remaining timeout, whichever
            // fires first. After wake we re-check status — Notify only
            // signals pre-existing waiters, so a status fetch is the
            // canonical source of truth.
            tokio::select! {
                _ = notifier.notified() => {}
                _ = tokio::time::sleep(remaining) => {
                    return self.approval_status(approval_id).await;
                }
            }
        }
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
            Err(CapError::BudgetExceeded {
                currency: self.currency.clone(),
                spent: total_after_increment,
                limit: self.amount_micro_units,
            })
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
        match limit.check(1_000_001) {
            Err(CapError::BudgetExceeded {
                currency,
                spent,
                limit,
            }) => {
                assert_eq!(currency, "USD");
                assert_eq!(spent, 1_000_001);
                assert_eq!(limit, 1_000_000);
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }
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

    #[tokio::test]
    async fn approval_decide_rejects_double_decide() {
        let st = InMemoryCaveatState::new();
        st.approval_request("a", "tok", "write", "/x")
            .await
            .unwrap();
        st.approval_decide("a", ApprovalDecision::Allow)
            .await
            .unwrap();
        // Second decide must fail — no flipping a denied to an allow.
        let res = st.approval_decide("a", ApprovalDecision::Deny).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn approval_decide_rejects_unknown_id() {
        let st = InMemoryCaveatState::new();
        let res = st.approval_decide("nope", ApprovalDecision::Allow).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn approval_wait_resumes_on_decide() {
        let st = Arc::new(InMemoryCaveatState::new());
        st.approval_request("a", "tok", "write", "/x")
            .await
            .unwrap();
        let st_b = st.clone();
        let waiter =
            tokio::spawn(async move { st_b.approval_wait("a", Duration::from_secs(5)).await });
        // Give the waiter a moment to park.
        tokio::time::sleep(Duration::from_millis(20)).await;
        st.approval_decide("a", ApprovalDecision::Allow)
            .await
            .unwrap();
        let res = waiter.await.unwrap().unwrap();
        assert_eq!(res, ApprovalDecision::Allow);
    }

    #[tokio::test]
    async fn approval_wait_returns_pending_on_timeout() {
        let st = InMemoryCaveatState::new();
        st.approval_request("a", "tok", "write", "/x")
            .await
            .unwrap();
        let res = st
            .approval_wait("a", Duration::from_millis(50))
            .await
            .unwrap();
        assert_eq!(res, ApprovalDecision::Pending);
    }

    #[tokio::test]
    async fn approval_wait_rejects_unknown_id() {
        let st = InMemoryCaveatState::new();
        let res = st.approval_wait("nope", Duration::from_millis(20)).await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn operation_cost_table_invariants() {
        // Read/subjects free.
        assert_eq!(OperationCost::from(Operation::Read), OperationCost::READ);
        // Compile-time invariants: any change to the cost table must
        // preserve these. A regression test in
        // `crates/ctxd-cap/tests/budget_cost_table.rs` covers the
        // runtime side; the const_assert! style here protects against
        // accidental rewrites of the constants themselves.
        const _: () = assert!(OperationCost::READ.0 == 0);
        const _: () = assert!(OperationCost::SUBJECTS.0 == 0);
        const _: () = assert!(OperationCost::WRITE.0 >= 1_000);
        const _: () = assert!(OperationCost::SEARCH.0 >= 1_000);
        const _: () = assert!(OperationCost::TIMELINE.0 > OperationCost::WRITE.0);
    }
}
