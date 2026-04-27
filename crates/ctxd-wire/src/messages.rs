//! Wire-protocol message types: [`Request`], [`Response`], and
//! [`BroadcastEvent`].
//!
//! These are the source-of-truth shapes that both daemon-side
//! `ProtocolServer` and any client SDK serialize via MessagePack. The
//! serialization is `serde`'s default for enums (externally tagged), so
//! the wire format is `{ "Pub": { ... } }`, `{ "Pong": null }`, etc.
//!
//! Event payloads (`PUB.data`, `PeerReplicate.event`, view results in
//! `Response::Ok.data`) are carried as `serde_json::Value` to keep the
//! protocol crate independent of `ctxd-core`. Consumers that want
//! strongly-typed events deserialize them in their own layer.

use serde::{Deserialize, Serialize};

/// Wire protocol request messages.
///
/// Externally-tagged: a `Request::Pub { ... }` serializes as
/// `{ "Pub": { "subject": "...", "event_type": "...", "data": ... } }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
// Federation verbs intentionally carry the `PeerCursorRequest` suffix to
// mirror the RFC-style PeerCursorRequest/PeerCursor pair (a request vs a
// carrier). The clippy lint is a style nudge we've chosen to reject here.
#[allow(clippy::enum_variant_names)]
pub enum Request {
    /// Publish (append) an event.
    Pub {
        /// Subject to publish under.
        subject: String,
        /// Event type discriminator.
        event_type: String,
        /// Opaque event payload.
        data: serde_json::Value,
    },
    /// Subscribe to events matching a subject pattern.
    Sub {
        /// Subject glob to match.
        subject_pattern: String,
    },
    /// Query a materialized view.
    Query {
        /// Subject glob to query.
        subject_pattern: String,
        /// View name (`log`, `kv`, `fts`).
        view: String,
    },
    /// Mint a capability token.
    Grant {
        /// Subject the token authorizes.
        subject: String,
        /// Operation strings (`read`, `write`, `subjects`, `search`, `admin`).
        operations: Vec<String>,
        /// Optional RFC3339 expiry timestamp.
        expiry: Option<String>,
    },
    /// Revoke a capability token (v0.2 stub).
    Revoke {
        /// Capability id to revoke.
        cap_id: String,
    },
    /// Health check.
    Ping,

    // --- v0.3 federation (2A) ---
    //
    // The federation verbs are wire-level types introduced in v0.3. A
    // handler for each is wired via `ctxd-cli/src/federation.rs`; for
    // daemons without federation enabled, the server returns a
    // structured `Response::Error` so the caller can detect.
    /// A peer introduces itself. Includes its Ed25519 public key, the
    /// capability it's offering the remote peer (base64-encoded biscuit),
    /// and the subject globs the remote should deliver to it.
    PeerHello {
        /// Local peer id (typically remote pubkey hex).
        peer_id: String,
        /// Sender's Ed25519 public key (32 raw bytes).
        public_key: Vec<u8>,
        /// Capability token sender mints for recipient (base64-encoded).
        offered_cap: String,
        /// Subject globs sender will deliver to recipient.
        subjects: Vec<String>,
    },

    /// Remote's welcome response with its reciprocal capability.
    PeerWelcome {
        /// Remote peer id.
        peer_id: String,
        /// Remote's Ed25519 public key.
        public_key: Vec<u8>,
        /// Reciprocal capability token (base64-encoded).
        offered_cap: String,
        /// Subject globs remote will deliver to sender.
        subjects: Vec<String>,
    },

    /// Streaming replication message carrying an event from peer.
    PeerReplicate {
        /// Origin peer id of the event.
        origin_peer_id: String,
        /// Serialized `ctxd_core::event::Event` as JSON.
        event: serde_json::Value,
    },

    /// Acknowledgement of a `PeerReplicate`, used to advance cursors.
    PeerAck {
        /// Origin peer id of the event being ACKed.
        origin_peer_id: String,
        /// UUIDv7 event id that was accepted.
        event_id: String,
    },

    /// Request the peer's current cursor for a subject pattern, to
    /// resume replication after a disconnect.
    PeerCursorRequest {
        /// Peer id whose cursor we're asking about.
        peer_id: String,
        /// Subject glob pattern.
        subject_pattern: String,
    },

    /// Carrier for a cursor response.
    PeerCursor {
        /// Peer id the cursor belongs to.
        peer_id: String,
        /// Subject glob pattern.
        subject_pattern: String,
        /// Last-known event id, or `None` if no events have been
        /// exchanged for this pattern.
        last_event_id: Option<String>,
        /// RFC3339 timestamp of the last-known event, or `None`.
        last_event_time: Option<String>,
    },

    /// Request a batch of events by id — used for parent-backfill when
    /// an incoming `PeerReplicate` references parents we don't have.
    PeerFetchEvents {
        /// UUIDv7 event ids to fetch.
        event_ids: Vec<String>,
    },
}

/// Wire protocol response messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Successful response with a JSON payload.
    Ok {
        /// Response payload.
        data: serde_json::Value,
    },
    /// An event streamed from a subscription.
    Event {
        /// Serialized event.
        event: serde_json::Value,
    },
    /// Error response.
    Error {
        /// Human-readable error message.
        message: String,
    },
    /// Pong response to a health check.
    Pong,
    /// End of stream marker.
    EndOfStream,
}

/// Broadcast event for SUB fan-out and federation replay.
///
/// `origin_peer_id` carries the peer-id of the daemon that *originally*
/// published the event (locally produced events use the local
/// daemon's peer_id; replicated-in events carry the origin from the
/// `PeerReplicate` envelope). Federation's outbound replicator uses
/// this for the loop-guard rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastEvent {
    /// Subject of the event.
    pub subject: String,
    /// JSON-serialized event payload.
    pub event: serde_json::Value,
    /// Origin peer id. Defaults to empty string for local PUB; the
    /// federation receiver overrides this with the inbound
    /// `PeerReplicate.origin_peer_id` before re-broadcasting.
    #[serde(default)]
    pub origin_peer_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialization_roundtrip() {
        let req = Request::Pub {
            subject: "/test/hello".to_string(),
            event_type: "demo".to_string(),
            data: serde_json::json!({"msg": "world"}),
        };
        let bytes = rmp_serde::to_vec(&req).expect("encode");
        let decoded: Request = rmp_serde::from_slice(&bytes).expect("decode");
        match decoded {
            Request::Pub {
                subject,
                event_type,
                data,
            } => {
                assert_eq!(subject, "/test/hello");
                assert_eq!(event_type, "demo");
                assert_eq!(data, serde_json::json!({"msg": "world"}));
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn response_serialization_roundtrip() {
        let resp = Response::Ok {
            data: serde_json::json!({"id": "abc123"}),
        };
        let bytes = rmp_serde::to_vec(&resp).expect("encode");
        let decoded: Response = rmp_serde::from_slice(&bytes).expect("decode");
        match decoded {
            Response::Ok { data } => {
                assert_eq!(data["id"], "abc123");
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn ping_pong_serialization() {
        let req = Request::Ping;
        let bytes = rmp_serde::to_vec(&req).expect("encode ping");
        let decoded: Request = rmp_serde::from_slice(&bytes).expect("decode ping");
        assert!(matches!(decoded, Request::Ping));

        let resp = Response::Pong;
        let bytes = rmp_serde::to_vec(&resp).expect("encode pong");
        let decoded: Response = rmp_serde::from_slice(&bytes).expect("decode pong");
        assert!(matches!(decoded, Response::Pong));
    }

    #[test]
    fn all_request_variants_serialize() {
        let variants: Vec<Request> = vec![
            Request::Ping,
            Request::Pub {
                subject: "/a".to_string(),
                event_type: "t".to_string(),
                data: serde_json::json!({}),
            },
            Request::Sub {
                subject_pattern: "/**".to_string(),
            },
            Request::Query {
                subject_pattern: "/a".to_string(),
                view: "log".to_string(),
            },
            Request::Grant {
                subject: "/**".to_string(),
                operations: vec!["read".to_string()],
                expiry: None,
            },
            Request::Revoke {
                cap_id: "id-1".to_string(),
            },
            Request::PeerHello {
                peer_id: "peer-a".to_string(),
                public_key: vec![1u8; 32],
                offered_cap: "Y2Fw".to_string(),
                subjects: vec!["/work/**".to_string()],
            },
            Request::PeerWelcome {
                peer_id: "peer-b".to_string(),
                public_key: vec![2u8; 32],
                offered_cap: "Y2Fw".to_string(),
                subjects: vec!["/home/**".to_string()],
            },
            Request::PeerReplicate {
                origin_peer_id: "peer-a".to_string(),
                event: serde_json::json!({"id": "01"}),
            },
            Request::PeerAck {
                origin_peer_id: "peer-a".to_string(),
                event_id: "01".to_string(),
            },
            Request::PeerCursorRequest {
                peer_id: "peer-a".to_string(),
                subject_pattern: "/**".to_string(),
            },
            Request::PeerCursor {
                peer_id: "peer-a".to_string(),
                subject_pattern: "/**".to_string(),
                last_event_id: Some("abc".to_string()),
                last_event_time: Some("2025-01-01T00:00:00Z".to_string()),
            },
            Request::PeerFetchEvents {
                event_ids: vec!["abc".to_string(), "def".to_string()],
            },
        ];
        for v in &variants {
            let bytes = rmp_serde::to_vec(v).expect("encode variant");
            let _: Request = rmp_serde::from_slice(&bytes).expect("decode variant");
        }
    }
}
