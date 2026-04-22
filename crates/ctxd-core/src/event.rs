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
/// ctxd extensions: `predecessorhash`, `signature`.
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

    /// Ed25519 signature. Reserved for v0.2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl Event {
    /// Create a new event with sensible defaults.
    ///
    /// Sets `specversion` to "1.0", generates a UUIDv7 `id`,
    /// sets `time` to now, and `datacontenttype` to "application/json".
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
        }
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
        // predecessorhash should be absent when None
        assert!(value.get("predecessorhash").is_none());
        assert!(value.get("signature").is_none());
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
}
