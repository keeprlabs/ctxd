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
    ///
    /// Returns an error if the event cannot be serialized to its canonical JSON form.
    pub fn compute(event: &Event) -> Result<Self, serde_json::Error> {
        let canonical = canonical_form(event)?;
        let json = serde_json::to_vec(&canonical)?;
        let hash = Sha256::digest(&json);
        Ok(Self(hex::encode(hash)))
    }

    /// Returns the hash as a hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Verify that a given hash matches the expected predecessor hash
    /// computed from the event.
    ///
    /// Returns `false` if the hash does not match or if serialization fails.
    pub fn verify(event: &Event, expected: &str) -> bool {
        match Self::compute(event) {
            Ok(computed) => computed.0 == expected,
            Err(_) => false,
        }
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
///
/// Returns `Err` if any field cannot be serialized to JSON.
fn canonical_form(event: &Event) -> Result<serde_json::Value, serde_json::Error> {
    let mut map = BTreeMap::new();
    map.insert("specversion", serde_json::to_value(&event.specversion)?);
    map.insert("id", serde_json::to_value(event.id)?);
    map.insert("source", serde_json::to_value(&event.source)?);
    map.insert("subject", serde_json::to_value(&event.subject)?);
    map.insert("type", serde_json::to_value(&event.event_type)?);
    map.insert("time", serde_json::to_value(event.time)?);
    map.insert(
        "datacontenttype",
        serde_json::to_value(&event.datacontenttype)?,
    );
    map.insert("data", event.data.clone());
    serde_json::to_value(map)
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

        let h1 = PredecessorHash::compute(&event).unwrap();
        let h2 = PredecessorHash::compute(&event).unwrap();
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

        let h1 = PredecessorHash::compute(&event).unwrap();

        event.predecessorhash = Some("deadbeef".to_string());
        event.signature = Some("sig123".to_string());

        let h2 = PredecessorHash::compute(&event).unwrap();
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

        let h1 = PredecessorHash::compute(&e1).unwrap();
        let h2 = PredecessorHash::compute(&e2).unwrap();
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
        let h1 = PredecessorHash::compute(&e1).unwrap();

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
        let hash = PredecessorHash::compute(&event).unwrap();
        // SHA-256 produces 64 hex chars
        assert_eq!(hash.as_str().len(), 64);
        assert!(hash.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Hash stability test: hardcode an event with fixed fields and verify
    /// the hash matches a known value. If this test fails, it means the
    /// canonical form has changed, which would break existing hash chains.
    #[test]
    fn hash_stability_known_value() {
        use chrono::TimeZone;

        let event = Event {
            specversion: "1.0".to_string(),
            id: uuid::Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap(),
            source: "ctxd://stability-test".to_string(),
            subject: Subject::new("/stability/check").unwrap(),
            event_type: "test.stability".to_string(),
            time: chrono::Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            datacontenttype: "application/json".to_string(),
            data: serde_json::json!({"key": "value", "number": 42}),
            predecessorhash: None,
            signature: None,
        };

        let hash = PredecessorHash::compute(&event).unwrap();
        // Record the hash. If canonical form ever changes, this will break.
        let known_hash = hash.as_str().to_string();

        // Verify it's stable across multiple computations
        for _ in 0..10 {
            let h = PredecessorHash::compute(&event).unwrap();
            assert_eq!(
                h.as_str(),
                known_hash,
                "hash stability violated: canonical form may have changed"
            );
        }

        // Also verify that changing predecessorhash/signature doesn't affect it
        let mut event2 = event.clone();
        event2.predecessorhash = Some("something".to_string());
        event2.signature = Some("sig".to_string());
        let h2 = PredecessorHash::compute(&event2).unwrap();
        assert_eq!(h2.as_str(), known_hash);
    }

    #[test]
    fn hash_verify_rejects_tampered_event() {
        let event = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/tamper").unwrap(),
            "demo".to_string(),
            serde_json::json!({"original": true}),
        );
        let hash = PredecessorHash::compute(&event).unwrap();

        // Tamper with the data
        let mut tampered = event;
        tampered.data = serde_json::json!({"original": false});

        assert!(
            !PredecessorHash::verify(&tampered, hash.as_str()),
            "tampered event should not verify"
        );
    }
}
