//! Capability-based authorization for ctxd using biscuit tokens.
//!
//! Each operation is authorized by a signed, attenuable token. Capabilities
//! use the biscuit-auth format with ctxd-specific facts for subject paths
//! and operation kinds.
//!
//! For v0.1: grant/verify/attenuate. Revocation is v0.2.

use biscuit_auth::builder::{Algorithm, AuthorizerBuilder, BlockBuilder};
use biscuit_auth::macros::*;
use biscuit_auth::{Biscuit, KeyPair, PrivateKey, PublicKey};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
            Self::Subjects => write!(f, "subjects"),
            Self::Search => write!(f, "search"),
            Self::Admin => write!(f, "admin"),
        }
    }
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

    /// Mint a new capability token.
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
        validate_datalog_safe(subject_glob, "subject_glob")?;
        if let Some(kinds) = kind_allowed {
            for kind in kinds {
                validate_datalog_safe(kind, "kind_allowed")?;
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
