//! Adversarial tests for the AES-256-GCM token-at-rest encryption.

use ctxd_adapter_gmail::crypto::{decrypt, encrypt, generate_master_key, CryptoError};

#[test]
fn round_trip() {
    let key = generate_master_key();
    let plaintext = b"refresh-token-with-some-entropy-XXXXX";
    let blob = encrypt(&key, plaintext).expect("encrypt");
    let recovered = decrypt(&key, &blob).expect("decrypt");
    assert_eq!(plaintext.to_vec(), recovered);
}

#[test]
fn ciphertext_swap_fails() {
    let key = generate_master_key();
    let plaintext = b"refresh-token";
    let mut blob = encrypt(&key, plaintext).unwrap();
    // Swap two bytes in the ciphertext region.
    let len = blob.len();
    blob.swap(len - 5, len - 6);
    let err = decrypt(&key, &blob).unwrap_err();
    assert!(matches!(err, CryptoError::Authentication));
}

#[test]
fn tag_swap_fails() {
    let key = generate_master_key();
    let plaintext = b"refresh-token";
    let mut blob = encrypt(&key, plaintext).unwrap();
    // Tamper with the tag (last 16 bytes).
    let last = blob.len() - 1;
    blob[last] ^= 0xAA;
    let err = decrypt(&key, &blob).unwrap_err();
    assert!(matches!(err, CryptoError::Authentication));
}

#[test]
fn wrong_key_fails() {
    let key_a = generate_master_key();
    let key_b = generate_master_key();
    let plaintext = b"refresh-token";
    let blob = encrypt(&key_a, plaintext).unwrap();
    let err = decrypt(&key_b, &blob).unwrap_err();
    assert!(matches!(err, CryptoError::Authentication));
}

#[test]
fn truncated_blob_rejected() {
    let key = generate_master_key();
    // Way too short.
    let blob = vec![0u8; 5];
    let err = decrypt(&key, &blob).unwrap_err();
    assert!(matches!(err, CryptoError::Truncated));
}

#[test]
fn ciphertext_does_not_leak_plaintext() {
    let key = generate_master_key();
    let plaintext = b"super-secret-refresh-token-AAAA";
    let blob = encrypt(&key, plaintext).unwrap();
    // The plaintext substring must not appear anywhere in the ciphertext.
    assert!(
        !blob.windows(plaintext.len()).any(|w| w == plaintext),
        "ciphertext leaked plaintext bytes"
    );
}
