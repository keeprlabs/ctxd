//! CloudEvents v1.0 compliant event type with ctxd extensions.
//!
//! We model our own struct rather than using the `cloudevents-sdk` crate
//! to maintain tight control over serialization (critical for hash chain
//! integrity) and to keep the dependency surface minimal.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::subject::Subject;

/// A CloudEvents v1.0 event with ctxd extensions.
///
/// Required CloudEvents attributes: `specversion`, `id`, `source`, `type`.
/// Optional CloudEvents attributes: `subject`, `time`, `datacontenttype`, `data`.
/// ctxd extensions: `predecessorhash`, `signature`, `parents`, `attestation`.
///
/// # Canonical form (v0.3)
///
/// For hashing and signing purposes, the canonical form includes `parents`
/// (always, as an array of UUID strings sorted lexicographically — empty
/// array if none) and `attestation` (hex-encoded bytes if present, `null`
/// otherwise). See [`crate::hash::PredecessorHash`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// CloudEvents spec version. Always "1.0".
    pub specversion: String,

    /// Unique event identifier. UUIDv7 for time-ordering.
    pub id: Uuid,

    /// Identifies the context in which the event happened.
    /// For ctxd, this is typically the daemon instance URI.
    pub source: String,

    /// The subject path this event is filed under.
    /// Uses path syntax: `/work/acme/customers/cust-42`
    pub subject: Subject,

    /// Event type descriptor, e.g. "ctx.note", "ctx.document", "demo".
    #[serde(rename = "type")]
    pub event_type: String,

    /// Timestamp of when the event was created.
    pub time: DateTime<Utc>,

    /// Content type of the data field. Defaults to "application/json".
    pub datacontenttype: String,

    /// The event payload.
    pub data: serde_json::Value,

    /// SHA-256 hash of the predecessor event's canonical form,
    /// scoped per subject tree. `None` for the first event in a chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessorhash: Option<String>,

    /// Ed25519 signature over the canonical form, hex-encoded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Parent event IDs, used to represent concurrent/branch merges in a
    /// federated log. An empty vec means "no explicit parents" — the
    /// event's only predecessor is the subject-chain predecessor given
    /// by [`predecessorhash`](Self::predecessorhash).
    ///
    /// Serialized when non-empty; always included in canonical form
    /// (sorted by id string; empty array when no parents).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<Uuid>,

    /// Optional TEE (trusted execution environment) attestation payload.
    ///
    /// ctxd does not interpret this field — it is carried end-to-end
    /// through federation replication and may be verified by an
    /// operator-supplied hook on the consumer side. Serialized as a
    /// hex-encoded string when present; always included in canonical
    /// form (hex-encoded or `null`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "hex_bytes_opt"
    )]
    pub attestation: Option<Vec<u8>>,
}

/// Serde helper: encode `Option<Vec<u8>>` as an optional hex string.
mod hex_bytes_opt {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(b) => hex::encode(b).serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let opt: Option<String> = Option::deserialize(d)?;
        match opt {
            Some(s) => hex::decode(&s)
                .map(Some)
                .map_err(|e| serde::de::Error::custom(format!("invalid hex attestation: {e}"))),
            None => Ok(None),
        }
    }
}

impl Event {
    /// Create a new event with sensible defaults.
    ///
    /// Sets `specversion` to "1.0", generates a UUIDv7 `id`,
    /// sets `time` to now, and `datacontenttype` to "application/json".
    /// `parents` is empty and `attestation` is `None`.
    pub fn new(
        source: String,
        subject: Subject,
        event_type: String,
        data: serde_json::Value,
    ) -> Self {
        Self {
            specversion: "1.0".to_string(),
            id: Uuid::now_v7(),
            source,
            subject,
            event_type,
            time: Utc::now(),
            datacontenttype: "application/json".to_string(),
            data,
            predecessorhash: None,
            signature: None,
            parents: Vec::new(),
            attestation: None,
        }
    }

    /// Return the parent IDs sorted lexicographically by their string
    /// representation, without mutating the event.
    ///
    /// This ordering is what the canonical form uses, so external code
    /// that wants to reason about the canonical parent order can call
    /// this helper instead of reimplementing the sort.
    pub fn parents_sorted(&self) -> Vec<Uuid> {
        let mut ps = self.parents.clone();
        ps.sort_by_key(|a| a.to_string());
        ps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_roundtrip_serialization() {
        let subject = Subject::new("/test/hello").unwrap();
        let event = Event::new(
            "ctxd://localhost".to_string(),
            subject,
            "demo".to_string(),
            serde_json::json!({"msg": "world"}),
        );

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();

        assert_eq!(event.id, deserialized.id);
        assert_eq!(event.subject, deserialized.subject);
        assert_eq!(event.event_type, deserialized.event_type);
        assert_eq!(event.data, deserialized.data);
        assert_eq!(event.specversion, "1.0");
        assert!(deserialized.parents.is_empty());
        assert!(deserialized.attestation.is_none());
    }

    #[test]
    fn event_json_structure_matches_cloudevents() {
        let subject = Subject::new("/test/path").unwrap();
        let event = Event::new(
            "ctxd://localhost".to_string(),
            subject,
            "demo".to_string(),
            serde_json::json!({"key": "value"}),
        );

        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["specversion"], "1.0");
        assert_eq!(value["type"], "demo");
        assert_eq!(value["datacontenttype"], "application/json");
        // Optional fields should be absent when empty/None.
        assert!(value.get("predecessorhash").is_none());
        assert!(value.get("signature").is_none());
        assert!(value.get("parents").is_none());
        assert!(value.get("attestation").is_none());
    }

    #[test]
    fn event_with_predecessor_hash() {
        let subject = Subject::new("/test/chain").unwrap();
        let mut event = Event::new(
            "ctxd://localhost".to_string(),
            subject,
            "demo".to_string(),
            serde_json::json!({}),
        );
        event.predecessorhash = Some("abc123".to_string());

        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["predecessorhash"], "abc123");
    }

    #[test]
    fn event_with_large_data_payload() {
        let large_string = "x".repeat(1_000_000);
        let data = serde_json::json!({"content": large_string});
        let subject = Subject::new("/test/large").unwrap();
        let event = Event::new(
            "ctxd://localhost".to_string(),
            subject,
            "demo".to_string(),
            data.clone(),
        );

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.data, data);
        assert!(json.len() > 1_000_000);
    }

    #[test]
    fn event_with_empty_data() {
        let subject = Subject::new("/test/empty").unwrap();
        let event = Event::new(
            "ctxd://localhost".to_string(),
            subject,
            "demo".to_string(),
            serde_json::json!(null),
        );

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.data, serde_json::Value::Null);

        let event2 = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/empty2").unwrap(),
            "demo".to_string(),
            serde_json::json!({}),
        );
        let json2 = serde_json::to_string(&event2).unwrap();
        let deser2: Event = serde_json::from_str(&json2).unwrap();
        assert_eq!(deser2.data, serde_json::json!({}));

        let event3 = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/test/empty3").unwrap(),
            "demo".to_string(),
            serde_json::json!([]),
        );
        let json3 = serde_json::to_string(&event3).unwrap();
        let deser3: Event = serde_json::from_str(&json3).unwrap();
        assert_eq!(deser3.data, serde_json::json!([]));
    }

    #[test]
    fn event_with_parents_and_attestation_roundtrips() {
        let mut event = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/merge/test").unwrap(),
            "demo".to_string(),
            serde_json::json!({"merge": true}),
        );
        event.parents = vec![Uuid::now_v7(), Uuid::now_v7()];
        event.attestation = Some(vec![0xde, 0xad, 0xbe, 0xef]);

        let json = serde_json::to_string(&event).unwrap();
        let deser: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.parents, event.parents);
        assert_eq!(
            deser.attestation.as_deref(),
            Some(&[0xde, 0xad, 0xbe, 0xefu8][..])
        );

        // Attestation should be serialized as hex
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["attestation"], "deadbeef");
        assert!(value["parents"].is_array());
    }

    #[test]
    fn parents_sorted_is_lexicographic() {
        let id_a = Uuid::parse_str("00000000-0000-7000-8000-000000000001").unwrap();
        let id_b = Uuid::parse_str("00000000-0000-7000-8000-000000000002").unwrap();
        let id_c = Uuid::parse_str("00000000-0000-7000-8000-000000000003").unwrap();

        let mut event = Event::new(
            "ctxd://localhost".to_string(),
            Subject::new("/sort/test").unwrap(),
            "demo".to_string(),
            serde_json::json!({}),
        );
        event.parents = vec![id_c, id_a, id_b];
        let sorted = event.parents_sorted();
        assert_eq!(sorted, vec![id_a, id_b, id_c]);
    }
}
