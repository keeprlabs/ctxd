//! Capability-based authorization for ctxd using biscuit tokens.
//!
//! Each operation is authorized by a signed, attenuable token. Capabilities
//! use the biscuit-auth format with ctxd-specific facts for subject paths
//! and operation kinds.
//!
//! For v0.1: grant/verify/attenuate. Revocation is v0.2. v0.3 adds the
//! [`state::CaveatState`] trait for budget + approval-style stateful
//! caveats, plus third-party-signed attenuation blocks (see
//! [`CapEngine::attenuate_with_block`] and [`CapEngine::verify_multi`]).

pub mod state;

use biscuit_auth::builder::{Algorithm, AuthorizerBuilder, BlockBuilder};
use biscuit_auth::macros::*;
use biscuit_auth::{Biscuit, KeyPair, PrivateKey, PublicKey};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

use crate::state::{ApprovalDecision, BudgetLimit, CaveatState, OperationCost};

// Re-exports so callers don't have to depend on `biscuit_auth` directly to
// pass third-party authority keys around. The two types are opaque from the
// outside and are only ever produced/consumed by the helper functions in
// this crate.
pub use biscuit_auth::{
    KeyPair as BiscuitKeyPair, PrivateKey as BiscuitPrivateKey, PublicKey as BiscuitPublicKey,
};

/// Errors from the capability engine.
#[derive(Debug, thiserror::Error)]
pub enum CapError {
    /// Biscuit token error.
    #[error("biscuit error: {0}")]
    Biscuit(#[from] biscuit_auth::error::Token),

    /// Authorization denied.
    #[error("authorization denied: {0}")]
    Denied(String),

    /// Base64 decoding error.
    #[error("base64 error: {0}")]
    Base64(String),

    /// A `BudgetLimit` caveat declared a cap that has been exceeded by
    /// the cumulative cost of all prior verifies plus this one.
    #[error("budget exceeded for {currency}: {spent} > {limit}")]
    BudgetExceeded {
        /// Currency code declared on the caveat.
        currency: String,
        /// Cumulative spend (post-increment) in micro-units.
        spent: i64,
        /// Declared cap in micro-units.
        limit: i64,
    },

    /// A `HumanApprovalRequired` caveat was satisfied with an explicit
    /// `Deny` decision.
    #[error("approval denied: {approval_id}")]
    ApprovalDenied {
        /// The approval id that was denied.
        approval_id: String,
    },

    /// A `HumanApprovalRequired` caveat was not decided within the
    /// verifier's timeout.
    #[error("approval timed out: {approval_id}")]
    ApprovalTimeout {
        /// The approval id that timed out.
        approval_id: String,
    },

    /// The token requires human approval but no [`state::CaveatState`]
    /// was supplied to [`CapEngine::verify_with_state`]. We refuse to
    /// silently downgrade — surfacing this lets callers fix their
    /// wiring rather than ship an unenforced caveat.
    #[error("token carries requires_approval but no CaveatState was provided to enforce it")]
    ApprovalStateMissing,
}

/// The operations that can be authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Read events.
    Read,
    /// Write (append) events.
    Write,
    /// List subjects.
    Subjects,
    /// Search events.
    Search,
    /// Admin operations (mint tokens, etc.).
    Admin,
    /// Federation peer-handshake / replication (v0.3). A token bearing
    /// `right(<subject_glob>, "peer")` authorizes the holder to participate
    /// in federation under that scope — handshake, replication and ack.
    Peer,
    /// Subscribe to events (v0.3 federation). A separate operation from
    /// `Read` so a peer cap can grant streaming without bulk-read.
    Subscribe,
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Subjects => write!(f, "subjects"),
            Self::Search => write!(f, "search"),
            Self::Admin => write!(f, "admin"),
            Self::Peer => write!(f, "peer"),
            Self::Subscribe => write!(f, "subscribe"),
        }
    }
}

/// The set of stateful caveats embedded in a token at mint time.
///
/// Returned by [`CapEngine::extract_stateful_caveats`]. Callers rarely
/// need to inspect this directly — [`CapEngine::verify_with_state`]
/// already consults it during enforcement — but tests and admin tools
/// use it to confirm that mint-time facts round-trip.
#[derive(Debug, Clone, Default)]
pub struct StatefulCaveats {
    /// The token's [`BudgetLimit`], if any. Only the *first*
    /// `budget_limit(...)` fact is honored — multi-currency budgets are
    /// out of scope for v0.3.
    pub budget_limit: Option<state::BudgetLimit>,
    /// Operations that require human approval before the verifier
    /// allows them through.
    pub requires_approval: Vec<Operation>,
}

/// A constraint that can be added to a capability via a third-party block.
///
/// `Caveat` is a small, declarative description of what an attenuating
/// authority wants to *narrow* on the underlying token. It is converted
/// to biscuit datalog inside [`CapEngine::attenuate_with_block`].
///
/// Each variant *narrows* — never widens — the token. The verifier
/// enforces this by requiring every block's checks to pass.
#[derive(Debug, Clone)]
pub enum Caveat {
    /// Restrict the subjects the bearer can act on. Acts as a prefix
    /// match (`/work/**` allows `/work/a` and `/work/a/b` but not
    /// `/home/x`).
    SubjectPrefix(String),
    /// Restrict the operations the bearer may perform to this set.
    OperationsAtMost(Vec<Operation>),
    /// Hard expiry — after this instant, the token must not authorize
    /// anything.
    ExpiresAt(DateTime<Utc>),
}

/// Validate a string is safe for interpolation into biscuit datalog.
/// Rejects strings containing characters that could break datalog syntax.
fn validate_datalog_safe(input: &str, field_name: &str) -> Result<(), CapError> {
    if input.contains('"') || input.contains(')') || input.contains(';') || input.contains('\n') {
        return Err(CapError::Denied(format!(
            "{field_name} contains invalid characters for capability tokens"
        )));
    }
    Ok(())
}

/// The capability engine. Holds the root key pair for minting tokens.
pub struct CapEngine {
    root_keypair: KeyPair,
}

impl Default for CapEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CapEngine {
    /// Create a new capability engine with a fresh root key pair.
    pub fn new() -> Self {
        Self {
            root_keypair: KeyPair::new(),
        }
    }

    /// Create a capability engine from an existing private key.
    pub fn from_private_key(key_bytes: &[u8]) -> Result<Self, CapError> {
        let private_key = PrivateKey::from_bytes(key_bytes, Algorithm::Ed25519)
            .map_err(|e| CapError::Denied(format!("invalid private key: {e}")))?;
        Ok(Self {
            root_keypair: KeyPair::from(&private_key),
        })
    }

    /// Get the root public key (for token verification).
    pub fn public_key(&self) -> PublicKey {
        self.root_keypair.public()
    }

    /// Get the root private key bytes (for persistence).
    pub fn private_key_bytes(&self) -> Vec<u8> {
        self.root_keypair.private().to_bytes().to_vec()
    }

    /// Mint a new capability token (legacy, no stateful caveats).
    ///
    /// Equivalent to [`Self::mint_full`] with `budget_limit = None` and
    /// `requires_approval = &[]`. Existing call sites that don't need
    /// budget or approval caveats keep working unchanged.
    ///
    /// # Parameters
    /// - `subject_glob`: Subject path glob pattern (e.g., "/**", "/test/**").
    /// - `operations`: The set of operations this token authorizes.
    /// - `expires_at`: Optional expiry time for the token.
    /// - `kind_allowed`: Optional list of event types this token is restricted to
    ///   (e.g., `["ctx.note"]`). If `None`, all event types are allowed.
    /// - `rate_limit_ops_per_sec`: Optional rate limit in operations per second.
    ///   For v0.1, this is stored as a fact in the token but **not enforced**.
    ///   Actual enforcement requires statefulness and is a v0.2 concern.
    pub fn mint(
        &self,
        subject_glob: &str,
        operations: &[Operation],
        expires_at: Option<DateTime<Utc>>,
        kind_allowed: Option<&[&str]>,
        rate_limit_ops_per_sec: Option<u32>,
    ) -> Result<Vec<u8>, CapError> {
        self.mint_full(
            subject_glob,
            operations,
            expires_at,
            kind_allowed,
            rate_limit_ops_per_sec,
            None,
            &[],
        )
    }

    /// Mint a new capability token with full v0.3 caveat support.
    ///
    /// In addition to the v0.2 caveats, this accepts:
    /// - `budget_limit`: emits a `budget_limit("<currency>", <amount>)`
    ///   fact. The amount is in micro-units (1 USD = 1_000_000). When
    ///   present, [`Self::verify_with_state`] charges per-operation
    ///   cost (see [`OperationCost`]) and rejects with
    ///   [`CapError::BudgetExceeded`] once the cap is breached.
    /// - `requires_approval`: emits one `requires_approval("<op>")` fact
    ///   per operation in the slice. When present,
    ///   [`Self::verify_with_state`] blocks the calling task on
    ///   [`CaveatState::approval_wait`] for the configured timeout
    ///   before allowing the op.
    // Caveat parameters are intentionally each their own argument so
    // call-sites read declaratively. Wrapping into a builder is on the
    // v0.4 backlog.
    #[allow(clippy::too_many_arguments)]
    pub fn mint_full(
        &self,
        subject_glob: &str,
        operations: &[Operation],
        expires_at: Option<DateTime<Utc>>,
        kind_allowed: Option<&[&str]>,
        rate_limit_ops_per_sec: Option<u32>,
        budget_limit: Option<&BudgetLimit>,
        requires_approval: &[Operation],
    ) -> Result<Vec<u8>, CapError> {
        validate_datalog_safe(subject_glob, "subject_glob")?;
        if let Some(kinds) = kind_allowed {
            for kind in kinds {
                validate_datalog_safe(kind, "kind_allowed")?;
            }
        }
        if let Some(b) = budget_limit {
            validate_datalog_safe(&b.currency, "budget_limit.currency")?;
            if b.amount_micro_units < 0 {
                return Err(CapError::Denied(
                    "budget_limit.amount_micro_units must be non-negative".to_string(),
                ));
            }
        }

        // Build using code() which parses a datalog string
        let mut facts = String::new();

        // Add a unique token_id fact for revocation support
        let token_id = Uuid::now_v7().to_string();
        facts.push_str(&format!("token_id(\"{token_id}\");\n"));

        for op in operations {
            facts.push_str(&format!("right(\"{subject_glob}\", \"{op}\");\n"));
        }

        // KindAllowed caveat: store allowed event types as facts
        if let Some(kinds) = kind_allowed {
            for kind in kinds {
                facts.push_str(&format!("kind_allowed(\"{kind}\");\n"));
            }
        }

        // RateLimit caveat: store as a fact in the token.
        // NOTE: Enforcement is a v0.2 feature — this fact is informational only.
        // A stateful rate-limiter would read this fact and enforce it at request time.
        if let Some(rate) = rate_limit_ops_per_sec {
            facts.push_str(&format!("rate_limit_ops_per_sec({rate});\n"));
        }

        // BudgetLimit caveat (v0.3). Stored as a fact; the verifier
        // resolves it against `CaveatState::budget_increment` at
        // verify time. Currency is already datalog-safe-validated.
        if let Some(b) = budget_limit {
            facts.push_str(&format!(
                "budget_limit(\"{}\", {});\n",
                b.currency, b.amount_micro_units
            ));
        }

        // HumanApprovalRequired caveat (v0.3). One fact per op so the
        // verifier can ask `requires_approval(<op>)` cheaply.
        for op in requires_approval {
            facts.push_str(&format!("requires_approval(\"{op}\");\n"));
        }

        let mut code = facts;
        if let Some(exp) = expires_at {
            let exp_secs = exp.timestamp();
            code.push_str(&format!("check if time($time), $time <= {exp_secs};\n"));
        }

        // If kind_allowed was specified, add a check that enforces it
        if kind_allowed.is_some() {
            code.push_str("check if event_type($etype), kind_allowed($etype);\n");
        }

        let builder = Biscuit::builder().code(&code)?;
        let biscuit = builder.build(&self.root_keypair)?;
        Ok(biscuit.to_vec()?)
    }

    /// Verify a capability token for a specific operation on a subject.
    ///
    /// This is the v0.2-compatible entry point: the verifier checks the
    /// static caveats (subject glob, operation, expiry, kind, third-party
    /// chain) but treats stateful caveats (`budget_limit`,
    /// `requires_approval`) as observed-but-not-enforced. To enforce
    /// them, use [`Self::verify_with_state`] and pass an
    /// `Arc<dyn CaveatState>`.
    ///
    /// # Parameters
    /// - `token`: The serialized biscuit token bytes.
    /// - `subject`: The subject path to verify access for.
    /// - `operation`: The operation to verify.
    /// - `event_type`: Optional event type for KindAllowed caveat verification.
    ///   If `None`, verifies without event type constraints (the KindAllowed
    ///   check in the token will use a wildcard match).
    pub fn verify(
        &self,
        token: &[u8],
        subject: &str,
        operation: Operation,
        event_type: Option<&str>,
    ) -> Result<(), CapError> {
        validate_datalog_safe(subject, "subject")?;
        if let Some(etype) = event_type {
            validate_datalog_safe(etype, "event_type")?;
        }

        let biscuit = Biscuit::from(token, self.root_keypair.public())?;
        let op_str = operation.to_string();
        let now = Utc::now().timestamp();

        // Build authorizer with policies via code string
        let mut auth_code = format!(
            r#"
            time({now});
            resource("{subject}");
            operation("{op_str}");
            "#
        );

        // If an event_type is provided, add it as a fact for KindAllowed checks.
        // If not provided, add a wildcard fact so that kind_allowed checks can
        // match if there is a kind_allowed fact present.
        if let Some(etype) = event_type {
            auth_code.push_str(&format!("event_type(\"{etype}\");\n"));
        } else {
            // When no event type is specified, we provide a synthetic event_type
            // that matches kind_allowed only if the token has no kind restriction.
            // We query facts below to check for kind_allowed presence.
        }

        auth_code.push_str(&format!(
            r#"
            allow if right("{subject}", "{op_str}");
            allow if right("/**", "{op_str}");
            "#
        ));

        let auth_builder = AuthorizerBuilder::new().code(&auth_code)?;
        let mut authorizer = auth_builder.build(&biscuit)?;
        if authorizer.authorize().is_ok() {
            return Ok(());
        }

        // Fallback: extract rights and do glob matching in Rust
        let base_code = format!(
            r#"
            time({now});
            resource("{subject}");
            operation("{op_str}");
            "#
        );

        let event_type_code = if let Some(etype) = event_type {
            format!("event_type(\"{etype}\");\n")
        } else {
            String::new()
        };

        let mut query_auth = biscuit.authorizer()?;
        let facts: Vec<(String, String)> =
            query_auth.query(rule!("data($sub, $op) <- right($sub, $op)"))?;

        // Also check for kind_allowed facts when no event_type is provided
        if event_type.is_none() {
            let kind_facts: Vec<(String,)> =
                query_auth.query(rule!("kind_data($k) <- kind_allowed($k)"))?;
            if !kind_facts.is_empty() {
                // Token has kind restrictions but no event_type was provided for
                // verification. We cannot satisfy the kind_allowed check.
                return Err(CapError::Denied(
                    "token has kind_allowed restriction but no event_type provided for verification"
                        .to_string(),
                ));
            }
        }

        for (pattern, fact_op) in &facts {
            if fact_op == &op_str && glob_match_subject(pattern, subject) {
                // Matched via glob. Re-authorize with injected right.
                let final_code = format!(
                    r#"
                    {base_code}
                    {event_type_code}
                    right("{subject}", "{op_str}");
                    allow if right("{subject}", "{op_str}");
                    "#
                );
                let final_builder = AuthorizerBuilder::new().code(&final_code)?;
                let mut final_auth = final_builder.build(&biscuit)?;
                final_auth.authorize()?;
                return Ok(());
            }
        }

        Err(CapError::Denied(format!(
            "no matching right for '{op_str}' on '{subject}'"
        )))
    }

    /// Verify a capability token, additionally enforcing stateful
    /// caveats against the supplied [`CaveatState`].
    ///
    /// # Stateful caveats
    /// - `budget_limit(currency, amount)`: every successful verify
    ///   charges the per-[`OperationCost`] cost for `operation` against
    ///   `(token_id, currency)` and rejects with
    ///   [`CapError::BudgetExceeded`] if the cumulative spend exceeds
    ///   `amount`. **The increment commits before the caller's op
    ///   runs** — see ADR 011 for the trade-off rationale.
    /// - `requires_approval(op)`: if a fact matches `operation`, the
    ///   verifier records a pending approval, blocks via
    ///   [`CaveatState::approval_wait`] for up to
    ///   `approval_timeout`, and returns
    ///   [`CapError::ApprovalDenied`] / [`CapError::ApprovalTimeout`]
    ///   on `Deny` / `Pending`.
    ///
    /// # Fallback semantics
    /// When `state` is `None`, behavior matches [`Self::verify`] —
    /// stateful caveats are observed (the static caveats still pass)
    /// but not enforced. Per ADR 011 this is intentional for tests and
    /// legacy call sites; a token that carries `requires_approval`
    /// will return [`CapError::ApprovalStateMissing`] when state is
    /// `None` to surface the wiring bug. Budget caveats degrade
    /// silently (return Ok) so v0.2 call sites keep working.
    ///
    /// # Tracing
    /// Approval-related events emit `tracing::info!` with structured
    /// fields (`token_id`, `approval_id`, `operation`, `subject`,
    /// `decision`). Tokens are never logged.
    pub async fn verify_with_state(
        &self,
        token: &[u8],
        subject: &str,
        operation: Operation,
        event_type: Option<&str>,
        state: Option<&dyn CaveatState>,
        approval_timeout: Duration,
    ) -> Result<(), CapError> {
        // 1. Static caveats first. If the token can't even pass these
        //    we don't want to charge a budget.
        self.verify(token, subject, operation, event_type)?;

        // 2. Extract stateful facts. The token's `token_id` is needed
        //    for both budget and approval bookkeeping; missing token_id
        //    means a pre-v0.2 token, which can't carry the new caveats
        //    in the first place — short-circuit Ok.
        let token_id = match self.extract_token_id(token)? {
            Some(id) => id,
            None => return Ok(()),
        };
        let stateful = self.extract_stateful_caveats(token)?;

        // Loud-fail when the token says "needs approval for op X" but
        // the verifier was given no state. We do *not* silently
        // downgrade — that's how production caveats stop being
        // enforced.
        if stateful.requires_approval.contains(&operation) && state.is_none() {
            return Err(CapError::ApprovalStateMissing);
        }

        let Some(state) = state else {
            // No state means the budget caveat is observed-but-not
            // -enforced (v0.2 fallback). The approval caveat already
            // returned `ApprovalStateMissing` above for any matching op.
            return Ok(());
        };

        // 3. Budget enforcement. Reserve-then-commit semantics: we
        //    increment first and bail on overshoot. The trade-off (see
        //    ADR 011): a downstream op that fails *after* verify will
        //    have already charged the budget. Refund is left to the
        //    caller via a future `budget_refund` API; for v0.3 we
        //    accept the over-conservatism.
        if let Some(budget) = stateful.budget_limit.as_ref() {
            let cost = OperationCost::from(operation).as_i64();
            // Skip the round-trip when cost is zero (read/subjects/…).
            if cost > 0 {
                let total = state
                    .budget_increment(&token_id, &budget.currency, cost)
                    .await?;
                budget.check(total)?;
            }
        }

        // 4. Approval enforcement.
        if stateful.requires_approval.contains(&operation) {
            let approval_id = Uuid::now_v7().to_string();
            let op_str = operation.to_string();
            tracing::info!(
                token_id = %token_id,
                approval_id = %approval_id,
                operation = %op_str,
                subject = %subject,
                "requesting human approval"
            );
            state
                .approval_request(&approval_id, &token_id, &op_str, subject)
                .await?;
            let decision = state.approval_wait(&approval_id, approval_timeout).await?;
            tracing::info!(
                token_id = %token_id,
                approval_id = %approval_id,
                operation = %op_str,
                decision = ?decision,
                "approval decision received"
            );
            match decision {
                ApprovalDecision::Allow => {}
                ApprovalDecision::Deny => {
                    return Err(CapError::ApprovalDenied { approval_id });
                }
                ApprovalDecision::Pending => {
                    return Err(CapError::ApprovalTimeout { approval_id });
                }
            }
        }

        Ok(())
    }

    /// Extract the `budget_limit` and `requires_approval` facts from a
    /// token. Used by [`Self::verify_with_state`] and by tests that
    /// want to assert the mint-time facts round-trip.
    pub fn extract_stateful_caveats(&self, token: &[u8]) -> Result<StatefulCaveats, CapError> {
        let biscuit = Biscuit::from(token, self.root_keypair.public())?;
        let mut authorizer = biscuit.authorizer()?;
        let budget_rows: Vec<(String, i64)> =
            authorizer.query(rule!("budget($c, $n) <- budget_limit($c, $n)"))?;
        let approval_rows: Vec<(String,)> =
            authorizer.query(rule!("approval($op) <- requires_approval($op)"))?;

        let budget_limit = budget_rows.into_iter().next().map(|(c, n)| BudgetLimit {
            currency: c,
            amount_micro_units: n,
        });

        let mut requires_approval = Vec::new();
        for (op,) in approval_rows {
            // Map the string back to the enum. Unknown ops are
            // silently dropped — they were minted by an older client
            // and we can't safely interpret them.
            let parsed = match op.as_str() {
                "read" => Some(Operation::Read),
                "write" => Some(Operation::Write),
                "subjects" => Some(Operation::Subjects),
                "search" => Some(Operation::Search),
                "admin" => Some(Operation::Admin),
                "peer" => Some(Operation::Peer),
                "subscribe" => Some(Operation::Subscribe),
                _ => None,
            };
            if let Some(op) = parsed {
                requires_approval.push(op);
            }
        }

        Ok(StatefulCaveats {
            budget_limit,
            requires_approval,
        })
    }

    /// Attenuate a token by adding restrictions.
    pub fn attenuate(
        &self,
        token: &[u8],
        subject_glob: &str,
        operations: &[Operation],
    ) -> Result<Vec<u8>, CapError> {
        validate_datalog_safe(subject_glob, "subject_glob")?;

        let biscuit = Biscuit::from(token, self.root_keypair.public())?;

        let ops_str: Vec<String> = operations.iter().map(|o| format!("\"{o}\"")).collect();
        let ops_set = ops_str.join(", ");
        let prefix = subject_glob.replace("/**", "").replace("/*", "");

        let block_code = format!(
            r#"
            check if operation($op), [{ops_set}].contains($op);
            check if resource($res), $res.starts_with("{prefix}");
            "#
        );

        let block = BlockBuilder::new().code(&block_code)?;
        let attenuated = biscuit.append(block)?;
        Ok(attenuated.to_vec()?)
    }

    /// Attenuate a token by appending a *third-party signed block*.
    ///
    /// Unlike [`Self::attenuate`], the appended block is signed with
    /// `authority_key` rather than the next biscuit-internal block
    /// keypair. This lets a downstream authority (B in `A → B → C`)
    /// add caveats that the verifier can attribute to B's pubkey
    /// using the `trusting <pubkey>` datalog clause.
    ///
    /// Each entry in `caveats` becomes a datalog `check if …` line in
    /// the third-party block. The block also emits a stable
    /// `attenuated_by("<pubkey-hex>")` fact so the verifier can confirm
    /// the chain in a single query without parsing block-internal
    /// signatures.
    ///
    /// The new token must subsequently be verified with
    /// [`Self::verify_multi`] using a trust set that contains the
    /// authority's public key — otherwise the third-party block's
    /// checks won't apply and the token will look like a plain
    /// (unattenuated) version on the wire.
    pub fn attenuate_with_block(
        &self,
        cap: &[u8],
        authority_key: &PrivateKey,
        caveats: &[Caveat],
    ) -> Result<Vec<u8>, CapError> {
        let biscuit = Biscuit::from(cap, self.root_keypair.public())?;
        let request = biscuit.third_party_request()?;

        let block_builder = build_caveat_block(authority_key, caveats)?;
        let block = request.create_block(authority_key, block_builder)?;
        let next = biscuit.append_third_party(KeyPair::from(authority_key).public(), block)?;
        Ok(next.to_vec()?)
    }

    /// Verify a token, additionally trusting third-party blocks signed
    /// by any of the public keys in `trusted_authorities`.
    ///
    /// The check is symmetric to [`Self::verify`]: we evaluate
    /// `right("<subject>", "<op>") trusting authority, <pk1>, <pk2>…`
    /// and require an `allow` from the authorizer. If a block is
    /// missing from the trust set, its caveats still apply *only if*
    /// they evaluate against the authority block — meaning a missing
    /// authority results in a *loud fail* on any subject/op the third
    /// party narrowed (rather than silently widening).
    ///
    /// `subject` and `op` follow the same rules as [`Self::verify`].
    pub fn verify_multi(
        &self,
        cap: &[u8],
        trusted_authorities: &[PublicKey],
        subject: &str,
        op: Operation,
    ) -> Result<(), CapError> {
        validate_datalog_safe(subject, "subject")?;
        let biscuit = Biscuit::from(cap, self.root_keypair.public())?;
        let op_str = op.to_string();
        let now = Utc::now().timestamp();

        // Loud-fail rule: every third-party block in the token must be
        // backed by a public key in `trusted_authorities`. A missing
        // authority means the verifier cannot evaluate that block's
        // caveats, which would silently widen — so we reject upfront.
        // (Biscuit-internal "first-party" blocks have `None` for the
        // external key and are always trusted via the authority chain.)
        for ext in biscuit.external_public_keys().into_iter().flatten() {
            if !trusted_authorities.contains(&ext) {
                return Err(CapError::Denied(format!(
                    "third-party block signed by {} is not in the trust set",
                    hex::encode(ext.to_bytes())
                )));
            }
        }

        // Build the authorizer datalog with one named scope param per
        // trusted authority pubkey, plus an `allow` policy that trusts
        // the authority block + every named external key.
        let mut scope_params: HashMap<String, PublicKey> = HashMap::new();
        let mut trusting_clause = String::from("authority");
        for (i, pk) in trusted_authorities.iter().enumerate() {
            let name = format!("ext{i}");
            scope_params.insert(name.clone(), *pk);
            trusting_clause.push_str(&format!(", {{{name}}}"));
        }

        // We emit:
        //   resource("…"); operation("…"); time(…);
        //   allow if right("<subj>", "<op>") trusting <authority + ext keys>;
        //   allow if right("/**",   "<op>") trusting <authority + ext keys>;
        //
        // …followed by glob-fallback re-authorization (mirrors
        // `verify`'s pattern) for prefix/wildcard matches the simple
        // datalog can't express directly.
        let direct_code = format!(
            r#"
            time({now});
            resource("{subject}");
            operation("{op_str}");
            allow if right("{subject}", "{op_str}") trusting {trusting_clause};
            allow if right("/**", "{op_str}") trusting {trusting_clause};
            "#
        );
        let auth_builder = AuthorizerBuilder::new().code_with_params(
            &direct_code,
            HashMap::new(),
            scope_params.clone(),
        )?;
        let mut authorizer = auth_builder.build(&biscuit)?;
        if authorizer.authorize().is_ok() {
            return Ok(());
        }

        // Fallback: extract `right` facts and do glob matching, then
        // re-authorize with an injected `right(<subj>, <op>)` fact.
        // Same authority+external trust set as the direct path so
        // third-party blocks remain in scope.
        let trusting_for_inject = trusting_clause.clone();
        let mut query_auth = biscuit.authorizer()?;
        let facts: Vec<(String, String)> =
            query_auth.query(rule!("data($sub, $op) <- right($sub, $op)"))?;
        for (pattern, fact_op) in &facts {
            if fact_op == &op_str && glob_match_subject(pattern, subject) {
                let final_code = format!(
                    r#"
                    time({now});
                    resource("{subject}");
                    operation("{op_str}");
                    right("{subject}", "{op_str}");
                    allow if right("{subject}", "{op_str}") trusting {trusting_for_inject};
                    "#
                );
                let final_builder = AuthorizerBuilder::new().code_with_params(
                    &final_code,
                    HashMap::new(),
                    scope_params,
                )?;
                let mut final_auth = final_builder.build(&biscuit)?;
                final_auth.authorize()?;
                return Ok(());
            }
        }

        Err(CapError::Denied(format!(
            "no matching right for '{op_str}' on '{subject}' under trust set of {} authorities",
            trusted_authorities.len()
        )))
    }

    /// Encode a token to base64 for transport.
    pub fn token_to_base64(token: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
    }

    /// Decode a token from base64.
    pub fn token_from_base64(encoded: &str) -> Result<Vec<u8>, CapError> {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|e| CapError::Base64(e.to_string()))
    }

    /// Extract the token_id from a biscuit token.
    ///
    /// Returns `None` if the token has no token_id fact (e.g., tokens minted
    /// before v0.2).
    pub fn extract_token_id(&self, token: &[u8]) -> Result<Option<String>, CapError> {
        let biscuit = Biscuit::from(token, self.root_keypair.public())?;
        let mut authorizer = biscuit.authorizer()?;
        let ids: Vec<(String,)> =
            authorizer.query(rule!("token_id_result($id) <- token_id($id)"))?;
        Ok(ids.into_iter().next().map(|(id,)| id))
    }

    /// Verify a token, also checking whether it has been revoked.
    ///
    /// `is_revoked` is a callback that checks whether a token_id has been
    /// revoked. This allows the caller to plug in any revocation store.
    pub fn verify_with_revocation<F>(
        &self,
        token: &[u8],
        subject: &str,
        operation: Operation,
        event_type: Option<&str>,
        is_revoked: F,
    ) -> Result<(), CapError>
    where
        F: FnOnce(&str) -> bool,
    {
        // Check revocation first
        if let Some(token_id) = self.extract_token_id(token)? {
            if is_revoked(&token_id) {
                return Err(CapError::Denied(format!(
                    "token {token_id} has been revoked"
                )));
            }
        }
        self.verify(token, subject, operation, event_type)
    }
}

/// Translate a slice of [`Caveat`]s into a third-party-signed
/// [`BlockBuilder`]. Each caveat becomes one or more datalog `check if`
/// lines plus a stable `attenuated_by(<pubkey-hex>)` fact identifying
/// the signing authority.
fn build_caveat_block(
    authority_key: &PrivateKey,
    caveats: &[Caveat],
) -> Result<BlockBuilder, CapError> {
    let pk_hex = hex::encode(KeyPair::from(authority_key).public().to_bytes());
    let mut code = String::new();
    code.push_str(&format!("attenuated_by(\"{pk_hex}\");\n"));

    for caveat in caveats {
        match caveat {
            Caveat::SubjectPrefix(prefix) => {
                validate_datalog_safe(prefix, "subject_prefix")?;
                let normalized = prefix.replace("/**", "").replace("/*", "");
                code.push_str(&format!(
                    "check if resource($res), $res.starts_with(\"{normalized}\");\n"
                ));
            }
            Caveat::OperationsAtMost(ops) => {
                let parts: Vec<String> = ops.iter().map(|o| format!("\"{o}\"")).collect();
                let set = parts.join(", ");
                code.push_str(&format!(
                    "check if operation($op), [{set}].contains($op);\n"
                ));
            }
            Caveat::ExpiresAt(t) => {
                let secs = t.timestamp();
                code.push_str(&format!("check if time($time), $time <= {secs};\n"));
            }
        }
    }

    BlockBuilder::new().code(&code).map_err(CapError::from)
}

/// Check if a subject matches a glob pattern.
fn glob_match_subject(pattern: &str, subject: &str) -> bool {
    if pattern == "/**" {
        return true;
    }
    if pattern == subject {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return subject == prefix || subject.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        if !subject.starts_with(&format!("{prefix}/")) {
            return false;
        }
        let rest = &subject[prefix.len() + 1..];
        return !rest.contains('/');
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_and_verify() {
        let engine = CapEngine::new();
        let token = engine
            .mint(
                "/**",
                &[Operation::Read, Operation::Write],
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .verify(&token, "/test/hello", Operation::Read, None)
            .unwrap();
        engine
            .verify(&token, "/test/hello", Operation::Write, None)
            .unwrap();
    }

    #[test]
    fn verify_rejects_wrong_operation() {
        let engine = CapEngine::new();
        let token = engine
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();
        assert!(engine
            .verify(&token, "/test/hello", Operation::Write, None)
            .is_err());
    }

    #[test]
    fn scoped_subject_pattern() {
        let engine = CapEngine::new();
        let token = engine
            .mint("/test/**", &[Operation::Read], None, None, None)
            .unwrap();
        engine
            .verify(&token, "/test/hello", Operation::Read, None)
            .unwrap();
        engine
            .verify(&token, "/test/a/b/c", Operation::Read, None)
            .unwrap();
        assert!(engine
            .verify(&token, "/other/hello", Operation::Read, None)
            .is_err());
    }

    #[test]
    fn base64_roundtrip() {
        let engine = CapEngine::new();
        let token = engine
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();
        let encoded = CapEngine::token_to_base64(&token);
        let decoded = CapEngine::token_from_base64(&encoded).unwrap();
        assert_eq!(token, decoded);
        engine
            .verify(&decoded, "/test/hello", Operation::Read, None)
            .unwrap();
    }

    #[test]
    fn expired_token_rejected() {
        let engine = CapEngine::new();
        let past = Utc::now() - chrono::Duration::hours(1);
        let token = engine
            .mint("/**", &[Operation::Read], Some(past), None, None)
            .unwrap();
        assert!(engine
            .verify(&token, "/test/hello", Operation::Read, None)
            .is_err());
    }

    #[test]
    fn private_key_persistence() {
        let engine = CapEngine::new();
        let key_bytes = engine.private_key_bytes();
        let engine2 = CapEngine::from_private_key(&key_bytes).unwrap();
        let token = engine
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();
        engine2
            .verify(&token, "/test/hello", Operation::Read, None)
            .unwrap();
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match_subject("/**", "/anything"));
        assert!(glob_match_subject("/test/**", "/test/hello"));
        assert!(glob_match_subject("/test/**", "/test/a/b/c"));
        assert!(!glob_match_subject("/test/**", "/other"));
        assert!(glob_match_subject("/test/*", "/test/hello"));
        assert!(!glob_match_subject("/test/*", "/test/a/b"));
        assert!(glob_match_subject("/test/hello", "/test/hello"));
        assert!(!glob_match_subject("/test/hello", "/test/other"));
    }

    #[test]
    fn attenuate_restricts_subject() {
        let engine = CapEngine::new();
        // Mint a /** token with read+write
        let token = engine
            .mint(
                "/**",
                &[Operation::Read, Operation::Write],
                None,
                None,
                None,
            )
            .unwrap();

        // Attenuate to /test/**
        let attenuated = engine
            .attenuate(&token, "/test/**", &[Operation::Read, Operation::Write])
            .unwrap();

        // /test/hello should work
        engine
            .verify(&attenuated, "/test/hello", Operation::Read, None)
            .unwrap();

        // /other/hello should fail
        assert!(engine
            .verify(&attenuated, "/other/hello", Operation::Read, None)
            .is_err());
    }

    #[test]
    fn attenuate_restricts_operations() {
        let engine = CapEngine::new();
        // Mint a token with read+write
        let token = engine
            .mint(
                "/**",
                &[Operation::Read, Operation::Write],
                None,
                None,
                None,
            )
            .unwrap();

        // Attenuate to read-only
        let attenuated = engine.attenuate(&token, "/**", &[Operation::Read]).unwrap();

        // Read should work
        engine
            .verify(&attenuated, "/test/hello", Operation::Read, None)
            .unwrap();

        // Write should fail
        assert!(engine
            .verify(&attenuated, "/test/hello", Operation::Write, None)
            .is_err());
    }

    #[test]
    fn kind_allowed_caveat() {
        let engine = CapEngine::new();
        // Mint a token restricted to "ctx.note" events only
        let token = engine
            .mint(
                "/**",
                &[Operation::Read, Operation::Write],
                None,
                Some(&["ctx.note"]),
                None,
            )
            .unwrap();

        // Verify with the correct event type should succeed
        engine
            .verify(&token, "/test/hello", Operation::Write, Some("ctx.note"))
            .unwrap();

        // Verify with a different event type should fail
        assert!(engine
            .verify(&token, "/test/hello", Operation::Write, Some("ctx.file"))
            .is_err());
    }

    #[test]
    fn datalog_injection_prevented() {
        let engine = CapEngine::new();

        // Try to mint with a malicious subject containing datalog injection
        let malicious_subject = r#"/**", "admin"); allow if true; //"#;
        let result = engine.mint(malicious_subject, &[Operation::Read], None, None, None);
        assert!(result.is_err(), "should reject malicious subject_glob");

        // Try injection via kind_allowed
        let malicious_kind = r#"ctx.note"); allow if true; //"#;
        let result = engine.mint(
            "/**",
            &[Operation::Read],
            None,
            Some(&[malicious_kind]),
            None,
        );
        assert!(result.is_err(), "should reject malicious kind_allowed");

        // Try injection via verify subject
        let token = engine
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();
        let result = engine.verify(&token, malicious_subject, Operation::Read, None);
        assert!(result.is_err(), "should reject malicious subject in verify");

        // Try injection via event_type
        let result = engine.verify(&token, "/test", Operation::Read, Some(malicious_kind));
        assert!(result.is_err(), "should reject malicious event_type");
    }

    #[test]
    fn rate_limit_fact_stored() {
        let engine = CapEngine::new();
        // Mint a token with a rate limit fact
        // This should succeed; enforcement is v0.2 but the fact is stored.
        let token = engine
            .mint("/**", &[Operation::Read], None, None, Some(100))
            .unwrap();

        // Token should still verify normally (rate limit is not enforced in v0.1)
        engine
            .verify(&token, "/test/hello", Operation::Read, None)
            .unwrap();
    }

    #[test]
    fn different_root_keys_cannot_cross_verify() {
        let engine_a = CapEngine::new();
        let engine_b = CapEngine::new();

        let token_a = engine_a
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();

        // engine_b should reject a token minted by engine_a
        assert!(
            engine_b
                .verify(&token_a, "/test/hello", Operation::Read, None)
                .is_err(),
            "token from engine A should not verify with engine B"
        );

        let token_b = engine_b
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();

        // engine_a should reject a token minted by engine_b
        assert!(
            engine_a
                .verify(&token_b, "/test/hello", Operation::Read, None)
                .is_err(),
            "token from engine B should not verify with engine A"
        );
    }

    #[test]
    fn attenuate_chain_root_to_scope_a_to_scope_b() {
        let engine = CapEngine::new();

        // Root token: /** with read+write
        let root_token = engine
            .mint(
                "/**",
                &[Operation::Read, Operation::Write],
                None,
                None,
                None,
            )
            .unwrap();

        // Attenuate to scope A: /work/**
        let scope_a = engine
            .attenuate(
                &root_token,
                "/work/**",
                &[Operation::Read, Operation::Write],
            )
            .unwrap();

        // Attenuate scope A to scope B: /work/team1/**
        let scope_b = engine
            .attenuate(&scope_a, "/work/team1/**", &[Operation::Read])
            .unwrap();

        // scope_b can read under /work/team1
        engine
            .verify(&scope_b, "/work/team1/doc", Operation::Read, None)
            .unwrap();

        // scope_b cannot write (was restricted to read-only)
        assert!(engine
            .verify(&scope_b, "/work/team1/doc", Operation::Write, None)
            .is_err());

        // scope_b cannot access outside /work/team1
        assert!(engine
            .verify(&scope_b, "/work/team2/doc", Operation::Read, None)
            .is_err());

        // scope_b cannot access outside /work
        assert!(engine
            .verify(&scope_b, "/other/doc", Operation::Read, None)
            .is_err());
    }

    #[test]
    fn datalog_injection_all_patterns_blocked() {
        let engine = CapEngine::new();

        // Test all injection characters in subjects during mint
        let injection_patterns: Vec<(&str, &str)> = vec![
            ("double quote", "/**\"; allow if true;//"),
            ("close paren", "/**); allow if true;//"),
            ("semicolon", "/**; allow if true"),
            ("newline", "/**\nallow if true"),
        ];

        for (desc, pattern) in &injection_patterns {
            assert!(
                engine
                    .mint(pattern, &[Operation::Read], None, None, None)
                    .is_err(),
                "mint should reject {desc} injection in subject_glob"
            );
        }

        // Test injection in kind_allowed
        let kind_injections: Vec<&str> = vec![
            "ctx.note\"; allow if true;//",
            "ctx.note); allow if true;//",
            "ctx.note; allow if true",
            "ctx.note\nallow if true",
        ];

        for malicious_kind in &kind_injections {
            assert!(
                engine
                    .mint(
                        "/**",
                        &[Operation::Read],
                        None,
                        Some(&[malicious_kind]),
                        None,
                    )
                    .is_err(),
                "mint should reject injection in kind_allowed: {malicious_kind}"
            );
        }

        // Test injection in verify subject
        let token = engine
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();
        for (desc, pattern) in &injection_patterns {
            assert!(
                engine
                    .verify(&token, pattern, Operation::Read, None)
                    .is_err(),
                "verify should reject {desc} injection in subject"
            );
        }

        // Test injection in verify event_type
        for malicious_etype in &kind_injections {
            assert!(
                engine
                    .verify(&token, "/test", Operation::Read, Some(malicious_etype))
                    .is_err(),
                "verify should reject injection in event_type: {malicious_etype}"
            );
        }
    }

    #[test]
    fn multiple_caveats_enforced_simultaneously() {
        let engine = CapEngine::new();

        // Mint a token with subject scope + kind restriction + expiry (future)
        let future_expiry = Utc::now() + chrono::Duration::hours(1);
        let token = engine
            .mint(
                "/work/**",
                &[Operation::Read, Operation::Write],
                Some(future_expiry),
                Some(&["ctx.note"]),
                None,
            )
            .unwrap();

        // All caveats satisfied: correct subject, correct op, correct kind, not expired
        engine
            .verify(&token, "/work/doc", Operation::Read, Some("ctx.note"))
            .unwrap();

        // Wrong subject (outside /work/**)
        assert!(engine
            .verify(&token, "/other/doc", Operation::Read, Some("ctx.note"))
            .is_err());

        // Wrong operation (admin not granted)
        assert!(engine
            .verify(&token, "/work/doc", Operation::Admin, Some("ctx.note"))
            .is_err());

        // Wrong kind
        assert!(engine
            .verify(&token, "/work/doc", Operation::Read, Some("ctx.file"))
            .is_err());

        // No event_type provided when kind_allowed is set
        assert!(engine
            .verify(&token, "/work/doc", Operation::Read, None)
            .is_err());

        // Expired token with all other caveats correct
        let past_expiry = Utc::now() - chrono::Duration::hours(1);
        let expired_token = engine
            .mint(
                "/work/**",
                &[Operation::Read],
                Some(past_expiry),
                Some(&["ctx.note"]),
                None,
            )
            .unwrap();
        assert!(engine
            .verify(
                &expired_token,
                "/work/doc",
                Operation::Read,
                Some("ctx.note")
            )
            .is_err());
    }

    #[test]
    fn attenuate_with_invalid_subject_rejected() {
        let engine = CapEngine::new();
        let token = engine
            .mint("/**", &[Operation::Read], None, None, None)
            .unwrap();

        // Injection in attenuate subject_glob
        assert!(engine
            .attenuate(&token, "/test\"; allow if true;//**", &[Operation::Read])
            .is_err());
    }
}
