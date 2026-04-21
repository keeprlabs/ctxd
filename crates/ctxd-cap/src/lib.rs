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
    pub fn mint(
        &self,
        subject_glob: &str,
        operations: &[Operation],
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<Vec<u8>, CapError> {
        // Build using code() which parses a datalog string
        let mut facts = String::new();
        for op in operations {
            facts.push_str(&format!("right(\"{subject_glob}\", \"{op}\");\n"));
        }

        let mut code = facts;
        if let Some(exp) = expires_at {
            let exp_secs = exp.timestamp();
            code.push_str(&format!("check if time($time), $time <= {exp_secs};\n"));
        }

        let builder = Biscuit::builder().code(&code)?;
        let biscuit = builder.build(&self.root_keypair)?;
        Ok(biscuit.to_vec()?)
    }

    /// Verify a capability token for a specific operation on a subject.
    pub fn verify(
        &self,
        token: &[u8],
        subject: &str,
        operation: Operation,
    ) -> Result<(), CapError> {
        let biscuit = Biscuit::from(token, self.root_keypair.public())?;
        let op_str = operation.to_string();
        let now = Utc::now().timestamp();

        // Build authorizer with policies via code string
        let auth_code = format!(
            r#"
            time({now});
            resource("{subject}");
            operation("{op_str}");
            allow if right("{subject}", "{op_str}");
            allow if right("/**", "{op_str}");
            "#
        );

        let auth_builder = AuthorizerBuilder::new().code(&auth_code)?;
        let mut authorizer = auth_builder.build(&biscuit)?;
        if authorizer.authorize().is_ok() {
            return Ok(());
        }

        // Fallback: extract rights and do glob matching in Rust
        let mut query_auth = biscuit.authorizer()?;
        let facts: Vec<(String, String)> =
            query_auth.query(rule!("data($sub, $op) <- right($sub, $op)"))?;

        for (pattern, fact_op) in &facts {
            if fact_op == &op_str && glob_match_subject(pattern, subject) {
                // Matched via glob. Re-authorize with injected right.
                let final_code = format!(
                    r#"
                    time({now});
                    resource("{subject}");
                    operation("{op_str}");
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
            .mint("/**", &[Operation::Read, Operation::Write], None)
            .unwrap();
        engine
            .verify(&token, "/test/hello", Operation::Read)
            .unwrap();
        engine
            .verify(&token, "/test/hello", Operation::Write)
            .unwrap();
    }

    #[test]
    fn verify_rejects_wrong_operation() {
        let engine = CapEngine::new();
        let token = engine.mint("/**", &[Operation::Read], None).unwrap();
        assert!(engine
            .verify(&token, "/test/hello", Operation::Write)
            .is_err());
    }

    #[test]
    fn scoped_subject_pattern() {
        let engine = CapEngine::new();
        let token = engine.mint("/test/**", &[Operation::Read], None).unwrap();
        engine
            .verify(&token, "/test/hello", Operation::Read)
            .unwrap();
        engine
            .verify(&token, "/test/a/b/c", Operation::Read)
            .unwrap();
        assert!(engine
            .verify(&token, "/other/hello", Operation::Read)
            .is_err());
    }

    #[test]
    fn base64_roundtrip() {
        let engine = CapEngine::new();
        let token = engine.mint("/**", &[Operation::Read], None).unwrap();
        let encoded = CapEngine::token_to_base64(&token);
        let decoded = CapEngine::token_from_base64(&encoded).unwrap();
        assert_eq!(token, decoded);
        engine
            .verify(&decoded, "/test/hello", Operation::Read)
            .unwrap();
    }

    #[test]
    fn expired_token_rejected() {
        let engine = CapEngine::new();
        let past = Utc::now() - chrono::Duration::hours(1);
        let token = engine.mint("/**", &[Operation::Read], Some(past)).unwrap();
        assert!(engine
            .verify(&token, "/test/hello", Operation::Read)
            .is_err());
    }

    #[test]
    fn private_key_persistence() {
        let engine = CapEngine::new();
        let key_bytes = engine.private_key_bytes();
        let engine2 = CapEngine::from_private_key(&key_bytes).unwrap();
        let token = engine.mint("/**", &[Operation::Read], None).unwrap();
        engine2
            .verify(&token, "/test/hello", Operation::Read)
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
}
