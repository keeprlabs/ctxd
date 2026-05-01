//! Stable response shapes for the admin HTTP API.
//!
//! These types are the wire contract for the v0.3 admin surface. They
//! are intentionally split out from [`crate::router`] so the v0.4
//! OpenAPI generator can mirror them without dragging in handler-only
//! code. Every field is rustdoc'd so the spec generator can pick up
//! descriptions verbatim.
//!
//! # Stability
//!
//! Adding a new optional field is non-breaking. Renaming or removing a
//! field is a breaking change and requires a wire-format version bump.

use ctxd_store::core::Peer;
use serde::{Deserialize, Serialize};

/// One peer in the response from `GET /v1/peers`.
///
/// Keys are deliberately renamed from the [`Peer`] struct so the JSON
/// stays stable even if the storage layer reshapes its internals:
///
/// - `granted_subjects` (storage) → `subject_patterns` (wire)
/// - `public_key` is hex-encoded for round-trippable JSON
///
/// `last_seen_at` is reserved for a future heartbeat column on the
/// `peers` table and is currently always `None`. Callers should treat
/// it as nullable today and not rely on a non-null value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerListItem {
    /// Local identifier for this peer. Often the hex-encoded remote
    /// public key, but free-form.
    pub peer_id: String,
    /// Address we dial when replicating with this peer
    /// (e.g. `tcp://host:port`).
    pub url: String,
    /// Remote peer's Ed25519 public key, hex-encoded (lowercase, 64
    /// chars).
    pub public_key: String,
    /// Subject globs we are willing to deliver to this peer.
    pub subject_patterns: Vec<String>,
    /// RFC3339 timestamp the peer was first registered.
    pub added_at: String,
    /// RFC3339 timestamp of the last successful exchange with this
    /// peer. Reserved for v0.4 heartbeat tracking; always `None` today.
    pub last_seen_at: Option<String>,
}

impl From<Peer> for PeerListItem {
    fn from(p: Peer) -> Self {
        Self {
            peer_id: p.peer_id,
            url: p.url,
            public_key: hex_lower(&p.public_key),
            subject_patterns: p.granted_subjects,
            added_at: p.added_at.to_rfc3339(),
            last_seen_at: None,
        }
    }
}

/// Response envelope for `GET /v1/peers`.
///
/// Wrapping the array in an object lets us add cursor / pagination
/// fields later without breaking clients that already deserialize the
/// top-level shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerListResponse {
    /// Peers in `(added_at ASC, peer_id ASC)` order.
    pub peers: Vec<PeerListItem>,
}

/// Opaque cursor for `/v1/events` pagination. Encodes the row's
/// SQLite `seq` so the next page is `seq < before`. base64-url with no
/// padding, JSON-shaped underneath so we can extend with extra fields
/// (e.g. subject filter snapshot) without breaking existing clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventsCursor {
    /// Sqlite seq of the row this cursor points BEFORE.
    pub seq: i64,
}

impl EventsCursor {
    /// Encode as base64-url-no-pad of the JSON form.
    pub fn encode(&self) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).unwrap_or_default())
    }

    /// Parse a cursor string. Returns `Err` on malformed input — the
    /// HTTP handler maps this to a 400. The error type carries no
    /// detail because the cursor is opaque to clients; "invalid
    /// cursor" is all we'd ever say.
    pub fn decode(s: &str) -> Result<Self, ()> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let bytes = URL_SAFE_NO_PAD.decode(s).map_err(|_| ())?;
        serde_json::from_slice(&bytes).map_err(|_| ())
    }
}

/// Lowercase hex encoding without external deps. Each byte → two
/// chars from `0123456789abcdef`.
fn hex_lower(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lower_basic() {
        assert_eq!(hex_lower(&[]), "");
        assert_eq!(hex_lower(&[0x00]), "00");
        assert_eq!(hex_lower(&[0xff]), "ff");
        assert_eq!(hex_lower(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        // 32-byte ed25519-shaped input.
        let bytes: Vec<u8> = (0..32).collect();
        assert_eq!(hex_lower(&bytes).len(), 64);
    }

    #[test]
    fn events_cursor_round_trip() {
        let c = EventsCursor { seq: 1247 };
        let encoded = c.encode();
        // base64-url-no-pad has no `=` padding and no `+`/`/`.
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        let back = EventsCursor::decode(&encoded).expect("decode");
        assert_eq!(back, c);
    }

    #[test]
    fn events_cursor_decode_rejects_garbage() {
        assert!(EventsCursor::decode("@@@invalid@@@").is_err());
        assert!(EventsCursor::decode("aGVsbG8").is_err()); // valid b64 but not JSON
    }

    #[test]
    fn peer_list_response_serializes_keys() {
        let resp = PeerListResponse {
            peers: vec![PeerListItem {
                peer_id: "p1".into(),
                url: "tcp://host:7778".into(),
                public_key: "00".repeat(32),
                subject_patterns: vec!["/a/*".into()],
                added_at: "2026-04-24T00:00:00+00:00".into(),
                last_seen_at: None,
            }],
        };
        let v = serde_json::to_value(&resp).expect("serialize");
        assert!(v["peers"].is_array());
        let item = &v["peers"][0];
        // Wire field is `subject_patterns`, not `granted_subjects`.
        assert!(item.get("subject_patterns").is_some());
        assert!(item.get("granted_subjects").is_none());
        // `last_seen_at` is present and null today.
        assert!(item["last_seen_at"].is_null());
    }
}
