//! Predecessor hash chain computation for tamper-evidence.
//!
//! Each event includes the SHA-256 hash of the previous event's canonical form,
//! scoped per subject tree. This creates a hash chain that proves no events
//! have been inserted, removed, or modified after the fact.
//!
//! ## Canonical form
//!
//! To compute the hash, we serialize the event to JSON with keys sorted
//! alphabetically (via `BTreeMap`-backed ordering). The `predecessorhash` and
//! `signature` fields are excluded from the canonical form to avoid circular
//! dependencies.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::event::Event;

/// Represents a SHA-256 predecessor hash as a hex string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredecessorHash(String);

impl PredecessorHash {
    /// Compute the predecessor hash for a given event.
    ///
    /// This produces the canonical form of the event (excluding `predecessorhash`
    /// and `signature`), serializes it to JSON with sorted keys, and computes
    /// the SHA-256 hash.
    pub fn compute(event: &Event) -> Self {
        let canonical = canonical_form(event);
        let json = serde_json::to_vec(&canonical).expect("BTreeMap serialization cannot fail");
        let hash = Sha256::digest(&json);
        Self(hex::encode(hash))
    }

    /// Returns the hash as a hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Verify that a given hash matches the expected predecessor hash
    /// computed from the event.
    pub fn verify(event: &Event, expected: &str) -> bool {
        let computed = Self::compute(event);
        computed.0 == expected
    }
}

impl std::fmt::Display for PredecessorHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<PredecessorHash> for String {
    fn from(h: PredecessorHash) -> String {
        h.0
    }
}

/// Produce the canonical form of an event for hashing.
///
/// Excludes `predecessorhash` and `signature` to avoid circular dependencies.
/// Uses a `BTreeMap` to guarantee sorted key order.
fn canonical_form(event: &Event) -> serde_json::Value {
    let mut map = BTreeMap::new();
    map.insert(
        "specversion",
        serde_json::to_value(&event.specversion).unwrap(),
    );
    map.insert("id", serde_json::to_value(event.id).unwrap());
    map.insert("source", serde_json::to_value(&event.source).unwrap());
    map.insert("subject", serde_json::to_value(&event.subject).unwrap());
    map.insert("type", serde_json::to_value(&event.event_type).unwrap());
    map.insert("time", serde_json::to_value(event.time).unwrap());
    map.insert(
        "datacontenttype",
        serde_json::to_value(&event.datacontenttype).unwrap(),
    );
    map.insert("data", event.data.clone());
    serde_json::to_value(map).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::Subject;

    #[test]
    fn hash_is_deterministic() {
        let event = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/hash").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let h1 = PredecessorHash::compute(&event);
        let h2 = PredecessorHash::compute(&event);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_excludes_predecessor_and_signature() {
        let subject = Subject::new("/test/hash").unwrap();
        let mut event = Event::new(
            "ctxd://localhost".to_string(),
            subject,
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let h1 = PredecessorHash::compute(&event);

        event.predecessorhash = Some("deadbeef".to_string());
        event.signature = Some("sig123".to_string());

        let h2 = PredecessorHash::compute(&event);
        assert_eq!(h1, h2, "predecessor and signature must not affect hash");
    }

    #[test]
    fn different_events_produce_different_hashes() {
        let e1 = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/a").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );
        let e2 = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/b").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let h1 = PredecessorHash::compute(&e1);
        let h2 = PredecessorHash::compute(&e2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_chain_integrity() {
        let e1 = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/chain").unwrap(),
            "demo".to_string(),
            serde_json::json!({"step": 1}),
        );
        let h1 = PredecessorHash::compute(&e1);

        let mut e2 = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/chain").unwrap(),
            "demo".to_string(),
            serde_json::json!({"step": 2}),
        );
        e2.predecessorhash = Some(h1.to_string());

        // Verify the chain link
        assert!(PredecessorHash::verify(
            &e1,
            e2.predecessorhash.as_ref().unwrap()
        ));
    }

    #[test]
    fn hash_is_valid_hex() {
        let event = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/hex").unwrap(),
            "demo".to_string(),
            serde_json::json!({}),
        );
        let hash = PredecessorHash::compute(&event);
        // SHA-256 produces 64 hex chars
        assert_eq!(hash.as_str().len(), 64);
        assert!(hash.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
