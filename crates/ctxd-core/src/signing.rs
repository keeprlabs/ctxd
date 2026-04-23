//! Ed25519 event signing and verification.
//!
//! Signs the canonical form of an event (same form used for predecessor hashing,
//! excluding `predecessorhash` and `signature` fields) using Ed25519.

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use std::collections::BTreeMap;

use crate::event::Event;

/// An event signer that holds an Ed25519 keypair.
pub struct EventSigner {
    signing_key: SigningKey,
}

impl EventSigner {
    /// Generate a fresh Ed25519 keypair.
    pub fn new() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self { signing_key }
    }

    /// Load an EventSigner from secret key bytes (32 bytes).
    pub fn from_bytes(secret: &[u8]) -> Result<Self, ed25519_dalek::SignatureError> {
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| ed25519_dalek::SignatureError::new())?;
        let signing_key = SigningKey::from_bytes(&bytes);
        Ok(Self { signing_key })
    }

    /// Return the secret key bytes (32 bytes).
    pub fn secret_key_bytes(&self) -> Vec<u8> {
        self.signing_key.to_bytes().to_vec()
    }

    /// Return the public key bytes (32 bytes).
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.signing_key.verifying_key().to_bytes().to_vec()
    }

    /// Sign the canonical form of an event. Returns a hex-encoded signature.
    pub fn sign(&self, event: &Event) -> String {
        let canonical = canonical_bytes(event);
        let signature = self.signing_key.sign(&canonical);
        hex::encode(signature.to_bytes())
    }

    /// Verify an event's signature against the given public key bytes.
    ///
    /// `signature` is hex-encoded. `public_key` is 32 raw bytes.
    pub fn verify(event: &Event, signature: &str, public_key: &[u8]) -> bool {
        let sig_bytes = match hex::decode(signature) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig_array: [u8; 64] = match sig_bytes.try_into() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let pk_array: [u8; 32] = match public_key.try_into() {
            Ok(a) => a,
            Err(_) => return false,
        };
        let sig = ed25519_dalek::Signature::from_bytes(&sig_array);
        let verifying_key = match VerifyingKey::from_bytes(&pk_array) {
            Ok(vk) => vk,
            Err(_) => return false,
        };
        let canonical = canonical_bytes(event);
        verifying_key.verify(&canonical, &sig).is_ok()
    }
}

impl Default for EventSigner {
    fn default() -> Self {
        Self::new()
    }
}

/// Produce canonical bytes for signing (same as hash canonical form).
fn canonical_bytes(event: &Event) -> Vec<u8> {
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
    serde_json::to_vec(&map).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subject::Subject;

    #[test]
    fn sign_and_verify() {
        let signer = EventSigner::new();
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/test/sign").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let sig = signer.sign(&event);
        let pubkey = signer.public_key_bytes();
        assert!(EventSigner::verify(&event, &sig, &pubkey));
    }

    #[test]
    fn tampered_event_fails_verification() {
        let signer = EventSigner::new();
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/test/sign").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let sig = signer.sign(&event);
        let pubkey = signer.public_key_bytes();

        // Tamper with the event
        let mut tampered = event.clone();
        tampered.data = serde_json::json!({"msg": "tampered"});
        assert!(!EventSigner::verify(&tampered, &sig, &pubkey));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let signer1 = EventSigner::new();
        let signer2 = EventSigner::new();
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/test/sign").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let sig = signer1.sign(&event);
        let wrong_pubkey = signer2.public_key_bytes();
        assert!(!EventSigner::verify(&event, &sig, &wrong_pubkey));
    }

    #[test]
    fn roundtrip_from_bytes() {
        let signer = EventSigner::new();
        let secret = signer.secret_key_bytes();
        let signer2 = EventSigner::from_bytes(&secret).unwrap();

        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/test/sign").unwrap(),
            "demo".to_string(),
            serde_json::json!({"msg": "hello"}),
        );

        let sig = signer.sign(&event);
        let pubkey = signer2.public_key_bytes();
        assert!(EventSigner::verify(&event, &sig, &pubkey));
    }

    #[test]
    fn invalid_signature_hex() {
        let event = Event::new(
            "ctxd://test".to_string(),
            Subject::new("/test/sign").unwrap(),
            "demo".to_string(),
            serde_json::json!({}),
        );
        let signer = EventSigner::new();
        let pubkey = signer.public_key_bytes();
        assert!(!EventSigner::verify(&event, "not-hex!", &pubkey));
        assert!(!EventSigner::verify(&event, "abcd", &pubkey));
    }
}
