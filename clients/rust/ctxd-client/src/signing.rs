//! Ed25519 signature verification.
//!
//! This module re-implements the daemon-side canonical-bytes routine
//! from `ctxd_core::signing` so SDK consumers can verify an event's
//! signature without taking a transitive dependency on the daemon's
//! own signer (which would otherwise pull `rand` and a side-effecting
//! `EventSigner::new`). The two implementations are pinned together by
//! the `docs/api/conformance/signatures/*.json` fixtures — if either
//! drifts, the conformance test in `tests/conformance.rs` breaks.

use std::collections::BTreeMap;

use ctxd_core::event::Event;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::errors::CtxdError;

/// Verify an event's Ed25519 signature against a hex-encoded public key.
///
/// `pubkey_hex` is a 64-character hex string (32 bytes). The event's
/// own [`Event::signature`] field is read; if it is `None`, this
/// returns `Ok(false)` rather than an error — an unsigned event is
/// indistinguishable from a tampered one for callers asking "is this
/// signed by `pubkey_hex`?".
///
/// Returns `Err(CtxdError::Signing)` only for hard input failures:
/// malformed hex, wrong-length pubkey, malformed signature hex, or a
/// canonical-form serialization failure (which in practice cannot
/// happen for well-formed [`Event`] values).
pub fn verify_signature(event: &Event, pubkey_hex: &str) -> Result<bool, CtxdError> {
    let pubkey_bytes = hex::decode(pubkey_hex.trim())
        .map_err(|e| CtxdError::Signing(format!("invalid pubkey hex: {e}")))?;
    let pk_array: [u8; 32] = pubkey_bytes
        .as_slice()
        .try_into()
        .map_err(|_| CtxdError::Signing("pubkey must be 32 bytes (64 hex chars)".to_string()))?;
    let verifying_key = VerifyingKey::from_bytes(&pk_array)
        .map_err(|e| CtxdError::Signing(format!("invalid pubkey: {e}")))?;

    let signature_hex = match event.signature.as_deref() {
        Some(s) => s,
        // Unsigned events: "is this signed by this key?" → No.
        None => return Ok(false),
    };

    let sig_bytes = hex::decode(signature_hex.trim())
        .map_err(|e| CtxdError::Signing(format!("invalid signature hex: {e}")))?;
    let sig_array: [u8; 64] = match sig_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return Ok(false), // wrong length → not this signature
    };
    let sig = Signature::from_bytes(&sig_array);

    let canonical = canonical_bytes(event)
        .map_err(|e| CtxdError::Signing(format!("canonical-form serialization: {e}")))?;

    Ok(verifying_key.verify(&canonical, &sig).is_ok())
}

/// Produce the canonical signing bytes for an event.
///
/// Mirrors `ctxd_core::signing::canonical_bytes` exactly. The
/// canonical form is a JSON object with **sorted keys** containing
/// every CloudEvents field except `predecessorhash` and `signature`,
/// plus the v0.3 `parents` (sorted lexicographically by string id) and
/// `attestation` (hex-encoded or `null`) fields.
fn canonical_bytes(event: &Event) -> Result<Vec<u8>, serde_json::Error> {
    let mut map: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
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

    // v0.3 fields — always present.
    let parents_sorted: Vec<String> = {
        let mut v: Vec<String> = event.parents.iter().map(|u| u.to_string()).collect();
        v.sort();
        v
    };
    map.insert("parents", serde_json::to_value(parents_sorted)?);

    let attestation_val: serde_json::Value = match &event.attestation {
        Some(bytes) => serde_json::Value::String(hex::encode(bytes)),
        None => serde_json::Value::Null,
    };
    map.insert("attestation", attestation_val);

    serde_json::to_vec(&map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxd_core::subject::Subject;

    #[test]
    fn unsigned_event_returns_false() {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/t/u").expect("subject"),
            "demo".to_string(),
            serde_json::json!({}),
        );
        // Any pubkey — unsigned events always return Ok(false).
        let pubkey = "00".repeat(32);
        let ok = verify_signature(&event, &pubkey).expect("verify");
        assert!(!ok);
    }

    #[test]
    fn malformed_pubkey_hex_errors() {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/t/u").expect("subject"),
            "demo".to_string(),
            serde_json::json!({}),
        );
        let err = verify_signature(&event, "not-hex!!").expect_err("must error");
        assert!(matches!(err, CtxdError::Signing(_)));
    }

    #[test]
    fn wrong_length_pubkey_errors() {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/t/u").expect("subject"),
            "demo".to_string(),
            serde_json::json!({}),
        );
        // 30 bytes instead of 32 — must be rejected.
        let short = "ab".repeat(30);
        let err = verify_signature(&event, &short).expect_err("must error");
        assert!(matches!(err, CtxdError::Signing(_)));
    }
}
