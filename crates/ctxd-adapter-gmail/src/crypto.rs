//! AES-256-GCM at-rest encryption for the Gmail refresh token.
//!
//! # Design
//!
//! - **Master key**: 32 random bytes generated on first `auth` call and
//!   persisted at `<state-dir>/gmail.key` with file mode 0600. The master
//!   key is the user's secret — anyone who can read it can decrypt the
//!   token.
//! - **Per-write key derivation**: HKDF-SHA256 over the master key with
//!   a fixed info string. This isolates the AES key from the persisted
//!   master key bytes; rotating to a per-write salt later is a non-
//!   breaking change because the salt is part of the file layout.
//! - **File layout**: `salt(16) || nonce(12) || ciphertext || tag(16)`.
//!   AES-GCM appends the 16-byte authentication tag onto the ciphertext.
//!   A random salt + nonce per write ensures we never reuse a (key,
//!   nonce) pair, which is a hard AES-GCM requirement.
//!
//! The token plaintext is never logged. All errors are surfaced as
//! `CryptoError::*` variants with no inner content.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;
use std::path::Path;

/// Length of the master key in bytes (256 bits).
pub const MASTER_KEY_LEN: usize = 32;

/// Length of the per-write HKDF salt in bytes.
pub const SALT_LEN: usize = 16;

/// Length of the AES-GCM nonce in bytes (96 bits, recommended).
pub const NONCE_LEN: usize = 12;

/// HKDF "info" context string. Including the adapter name + version-ish
/// tag makes the derived key domain-separated from any other use of the
/// master key.
const HKDF_INFO: &[u8] = b"ctxd-adapter-gmail/v1/aes-256-gcm";

/// AES-GCM authenticated additional data. Bound to the file format so a
/// ciphertext from a different format (or a different field) can't be
/// substituted.
const AEAD_AAD: &[u8] = b"ctxd-adapter-gmail/v1/token";

/// Errors produced by the crypto layer.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The on-disk file was shorter than the minimum (salt + nonce + tag).
    #[error("ciphertext file is truncated")]
    Truncated,

    /// HKDF expansion failed (only happens for absurd output lengths).
    #[error("key derivation failed")]
    KeyDerivation,

    /// AES-GCM authentication failed: the ciphertext, tag, salt, or
    /// master key was tampered with or is wrong.
    #[error("decryption failed: authentication tag mismatch")]
    Authentication,

    /// AES-GCM encryption returned an error (essentially infallible for
    /// well-formed inputs but kept for completeness).
    #[error("encryption failed")]
    Encryption,
}

/// Generate a new 32-byte master key from the OS RNG.
pub fn generate_master_key() -> [u8; MASTER_KEY_LEN] {
    let mut key = [0u8; MASTER_KEY_LEN];
    OsRng.fill_bytes(&mut key);
    key
}

/// Derive an AES-256 key from the master key + salt using HKDF-SHA256.
fn derive_aes_key(master_key: &[u8], salt: &[u8]) -> Result<[u8; 32], CryptoError> {
    let hk = Hkdf::<Sha256>::new(Some(salt), master_key);
    let mut okm = [0u8; 32];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| CryptoError::KeyDerivation)?;
    Ok(okm)
}

/// Encrypt `plaintext` with the master key.
///
/// Returns a self-contained byte string with layout
/// `salt || nonce || ciphertext || tag`.
pub fn encrypt(master_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if master_key.len() != MASTER_KEY_LEN {
        return Err(CryptoError::KeyDerivation);
    }

    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);

    let aes_key = derive_aes_key(master_key, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&aes_key).map_err(|_| CryptoError::KeyDerivation)?;
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: AEAD_AAD,
            },
        )
        .map_err(|_| CryptoError::Encryption)?;

    let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a `salt || nonce || ciphertext || tag` blob.
pub fn decrypt(master_key: &[u8], blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if master_key.len() != MASTER_KEY_LEN {
        return Err(CryptoError::KeyDerivation);
    }
    // AES-GCM tag is 16 bytes; the AEAD output is ciphertext || tag, so
    // the minimum total is salt + nonce + tag = 44 bytes (empty plaintext).
    if blob.len() < SALT_LEN + NONCE_LEN + 16 {
        return Err(CryptoError::Truncated);
    }

    let salt = &blob[..SALT_LEN];
    let nonce_bytes = &blob[SALT_LEN..SALT_LEN + NONCE_LEN];
    let ciphertext = &blob[SALT_LEN + NONCE_LEN..];

    let aes_key = derive_aes_key(master_key, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&aes_key).map_err(|_| CryptoError::KeyDerivation)?;
    let nonce_arr: [u8; NONCE_LEN] = nonce_bytes.try_into().map_err(|_| CryptoError::Truncated)?;
    let nonce = Nonce::from(nonce_arr);

    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: AEAD_AAD,
            },
        )
        .map_err(|_| CryptoError::Authentication)
}

/// Read a master key from disk, or generate + persist a new one if the
/// file does not exist. Sets file mode 0600 on Unix.
pub async fn load_or_create_master_key(path: &Path) -> std::io::Result<[u8; MASTER_KEY_LEN]> {
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.len() == MASTER_KEY_LEN => {
            let mut key = [0u8; MASTER_KEY_LEN];
            key.copy_from_slice(&bytes);
            Ok(key)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "master key file is the wrong length",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = generate_master_key();
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            write_secret_file(path, &key).await?;
            Ok(key)
        }
        Err(e) => Err(e),
    }
}

/// Read a master key from disk. Errors if the file does not exist.
pub async fn load_master_key(path: &Path) -> std::io::Result<[u8; MASTER_KEY_LEN]> {
    let bytes = tokio::fs::read(path).await?;
    if bytes.len() != MASTER_KEY_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "master key file is the wrong length",
        ));
    }
    let mut key = [0u8; MASTER_KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Write `bytes` to `path` with file mode 0600 on Unix.
///
/// Best-effort on non-Unix targets: we still write the bytes but rely on
/// the OS to honor a sensible default ACL.
pub async fn write_secret_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Write to a temp file then rename, so a crash mid-write can't leave
    // a half-baked secret on disk.
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(&tmp, perms).await?;
    }

    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_encrypts_and_decrypts() {
        let key = generate_master_key();
        let plaintext = b"refresh-token-value-very-secret";
        let blob = encrypt(&key, plaintext).expect("encrypt");
        let recovered = decrypt(&key, &blob).expect("decrypt");
        assert_eq!(plaintext.to_vec(), recovered);
    }

    #[test]
    fn each_encrypt_uses_fresh_salt_and_nonce() {
        let key = generate_master_key();
        let plaintext = b"hello world";
        let a = encrypt(&key, plaintext).unwrap();
        let b = encrypt(&key, plaintext).unwrap();
        assert_ne!(a, b, "encrypting the same plaintext twice must differ");
    }

    #[test]
    fn tamper_with_ciphertext_byte_fails() {
        let key = generate_master_key();
        let plaintext = b"refresh-token";
        let mut blob = encrypt(&key, plaintext).unwrap();
        // Flip a byte deep in the ciphertext (past salt + nonce).
        let idx = SALT_LEN + NONCE_LEN + 1;
        blob[idx] ^= 0xFF;
        let err = decrypt(&key, &blob).unwrap_err();
        assert!(matches!(err, CryptoError::Authentication));
    }

    #[test]
    fn tamper_with_tag_fails() {
        let key = generate_master_key();
        let plaintext = b"refresh-token";
        let mut blob = encrypt(&key, plaintext).unwrap();
        // The tag is the last 16 bytes (AES-GCM appends it after the
        // ciphertext). Flip the very last byte.
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
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
    fn truncated_blob_fails() {
        let key = generate_master_key();
        let blob = vec![0u8; 10];
        let err = decrypt(&key, &blob).unwrap_err();
        assert!(matches!(err, CryptoError::Truncated));
    }

    #[test]
    fn empty_plaintext_round_trips() {
        let key = generate_master_key();
        let blob = encrypt(&key, b"").unwrap();
        let recovered = decrypt(&key, &blob).unwrap();
        assert!(recovered.is_empty());
    }
}
