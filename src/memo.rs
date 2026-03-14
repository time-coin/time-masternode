//! Encrypted memo support for TIME Coin transactions.
//!
//! Memos are encrypted using ECDH key exchange (X25519) + AES-256-GCM so that
//! only the sender and recipient can decrypt them. Other nodes on the network
//! see only ciphertext.
//!
//! ## Wire format (encrypted_memo bytes)
//!
//! ```text
//! [0]       version byte (0x01)
//! [1..33]   sender's Ed25519 public key (32 bytes)
//! [33..65]  recipient's Ed25519 public key (32 bytes)
//! [65..77]  AES-GCM nonce (12 bytes)
//! [77..]    AES-GCM ciphertext + 16-byte auth tag
//! ```
//!
//! ## Key derivation
//!
//! 1. Convert sender's Ed25519 signing key → X25519 static secret
//! 2. Derive recipient's X25519 public key from their Ed25519 public key
//! 3. ECDH shared secret = X25519(sender_secret, recipient_public)
//! 4. Encryption key = SHA-256(shared_secret || "TIME-memo-v1")
//! 5. Encrypt plaintext memo with AES-256-GCM
//!
//! Both sender and recipient can reconstruct the shared secret:
//! - Sender uses X25519(sender_secret, recipient_public_from_blob)
//! - Recipient uses X25519(recipient_secret, sender_public_from_blob)

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret as X25519Secret};

const MEMO_VERSION: u8 = 0x01;
const MEMO_MAX_LEN: usize = 256;

/// Encrypt a memo so that both the sender and the recipient can decrypt it.
///
/// - `sender_key`: The sender's Ed25519 signing key (wallet key).
/// - `recipient_ed25519_pubkey`: The recipient's Ed25519 public key (32 bytes).
///   For self-sends (consolidation), pass the sender's own verifying key bytes.
/// - `plaintext`: The human-readable memo string (max 256 bytes).
///
/// Returns the encrypted memo blob ready to store in `Transaction::encrypted_memo`.
pub fn encrypt_memo(
    sender_key: &SigningKey,
    recipient_ed25519_pubkey: &[u8; 32],
    plaintext: &str,
) -> Result<Vec<u8>, MemoError> {
    if plaintext.is_empty() {
        return Err(MemoError::Empty);
    }
    if plaintext.len() > MEMO_MAX_LEN {
        return Err(MemoError::TooLong(plaintext.len()));
    }

    let sender_x25519 = ed25519_to_x25519_secret(sender_key);
    let recipient_x25519 = ed25519_to_x25519_public(recipient_ed25519_pubkey);

    let shared_secret = sender_x25519.diffie_hellman(&recipient_x25519);
    let enc_key = derive_aes_key(shared_secret.as_bytes());

    let cipher = Aes256Gcm::new_from_slice(&enc_key)
        .map_err(|_| MemoError::Encryption("AES key init failed".into()))?;

    let nonce_bytes: [u8; 12] = rand::Rng::gen(&mut rand::thread_rng());
    let nonce = Nonce::from(nonce_bytes);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| MemoError::Encryption("AES-GCM encrypt failed".into()))?;

    // Assemble wire format: version + sender_pubkey + recipient_pubkey + nonce + ciphertext
    let sender_pubkey = sender_key.verifying_key().to_bytes();
    let mut blob = Vec::with_capacity(1 + 32 + 32 + 12 + ciphertext.len());
    blob.push(MEMO_VERSION);
    blob.extend_from_slice(&sender_pubkey);
    blob.extend_from_slice(recipient_ed25519_pubkey);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);

    Ok(blob)
}

/// Try to decrypt a memo. Returns `Ok(Some(plaintext))` if this node holds
/// a key that can decrypt it, `Ok(None)` if the memo isn't for us, or
/// `Err` on format errors.
///
/// - `our_key`: This node's Ed25519 signing key.
/// - `encrypted`: The raw `encrypted_memo` bytes from the transaction.
pub fn decrypt_memo(our_key: &SigningKey, encrypted: &[u8]) -> Result<Option<String>, MemoError> {
    // Minimum: version(1) + sender_pubkey(32) + recipient_pubkey(32) + nonce(12) + tag(16)
    if encrypted.len() < 1 + 32 + 32 + 12 + 16 {
        return Err(MemoError::InvalidFormat("too short".into()));
    }

    let version = encrypted[0];
    if version != MEMO_VERSION {
        return Err(MemoError::InvalidFormat(format!(
            "unsupported version {}",
            version
        )));
    }

    let sender_pubkey: [u8; 32] = encrypted[1..33]
        .try_into()
        .map_err(|_| MemoError::InvalidFormat("bad sender pubkey".into()))?;
    let recipient_pubkey: [u8; 32] = encrypted[33..65]
        .try_into()
        .map_err(|_| MemoError::InvalidFormat("bad recipient pubkey".into()))?;
    let nonce_bytes: [u8; 12] = encrypted[65..77]
        .try_into()
        .map_err(|_| MemoError::InvalidFormat("bad nonce".into()))?;
    let ciphertext = &encrypted[77..];

    let our_pubkey = our_key.verifying_key().to_bytes();
    let our_x25519_secret = ed25519_to_x25519_secret(our_key);
    let nonce = Nonce::from(nonce_bytes);

    // Determine which role we play and compute the shared secret accordingly
    let peer_pubkey = if our_pubkey == recipient_pubkey {
        // We are the recipient: shared_secret = X25519(our_secret, sender_public)
        sender_pubkey
    } else if our_pubkey == sender_pubkey {
        // We are the sender: shared_secret = X25519(our_secret, recipient_public)
        recipient_pubkey
    } else {
        // We're neither sender nor recipient — memo isn't for us
        return Ok(None);
    };

    let peer_x25519_pub = ed25519_to_x25519_public(&peer_pubkey);
    let shared = our_x25519_secret.diffie_hellman(&peer_x25519_pub);
    let enc_key = derive_aes_key(shared.as_bytes());

    let cipher = Aes256Gcm::new_from_slice(&enc_key)
        .map_err(|_| MemoError::Encryption("AES key init failed".into()))?;

    match cipher.decrypt(&nonce, ciphertext) {
        Ok(plaintext) => {
            Ok(Some(String::from_utf8(plaintext).map_err(|_| {
                MemoError::InvalidFormat("memo not valid UTF-8".into())
            })?))
        }
        Err(_) => Ok(None),
    }
}

/// Convert an Ed25519 signing key to an X25519 static secret.
/// Ed25519 internally derives its scalar as SHA-512(seed)[0..32] with clamping.
/// We use the same derivation so the X25519 secret corresponds to the same
/// point as the Ed25519 public key (after birational map to Montgomery).
fn ed25519_to_x25519_secret(ed_key: &SigningKey) -> X25519Secret {
    use sha2::Sha512;
    let hash = Sha512::digest(ed_key.to_bytes());
    let mut x25519_bytes = [0u8; 32];
    x25519_bytes.copy_from_slice(&hash[..32]);
    X25519Secret::from(x25519_bytes)
}

/// Convert an Ed25519 public key (32 bytes) to an X25519 public key.
/// This uses the standard birational map from the Ed25519 curve (twisted Edwards)
/// to the Montgomery curve used by X25519.
fn ed25519_to_x25519_public(ed_pubkey: &[u8; 32]) -> X25519Public {
    // Use curve25519_dalek's built-in conversion
    let ed_point = curve25519_dalek::edwards::CompressedEdwardsY(*ed_pubkey);
    if let Some(point) = ed_point.decompress() {
        let montgomery = point.to_montgomery();
        X25519Public::from(montgomery.to_bytes())
    } else {
        // Fallback: hash-based derivation (should never happen with valid keys)
        let hash = Sha256::digest(ed_pubkey);
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&hash[..32]);
        X25519Public::from(bytes)
    }
}

/// Derive a 256-bit AES key from the ECDH shared secret using domain separation.
fn derive_aes_key(shared_secret: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(shared_secret);
    hasher.update(b"TIME-memo-v1");
    hasher.finalize().into()
}

#[derive(Debug, thiserror::Error)]
pub enum MemoError {
    #[error("memo is empty")]
    Empty,
    #[error("memo too long ({0} bytes, max {MEMO_MAX_LEN})")]
    TooLong(usize),
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("invalid memo format: {0}")]
    InvalidFormat(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn random_signing_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        SigningKey::from_bytes(&bytes)
    }

    #[test]
    fn test_self_send_roundtrip() {
        let key = random_signing_key();
        let pubkey = key.verifying_key().to_bytes();

        let memo = "UTXO Consolidation";
        let encrypted = encrypt_memo(&key, &pubkey, memo).unwrap();

        // Version byte + 32 sender pubkey + 32 recipient pubkey + 12 nonce + ciphertext + 16 tag
        assert!(encrypted.len() > 77);
        assert_eq!(encrypted[0], MEMO_VERSION);

        let decrypted = decrypt_memo(&key, &encrypted).unwrap();
        assert_eq!(decrypted, Some(memo.to_string()));
    }

    #[test]
    fn test_sender_recipient_roundtrip() {
        let sender = random_signing_key();
        let recipient = random_signing_key();
        let recipient_pubkey = recipient.verifying_key().to_bytes();

        let memo = "Payment for services";
        let encrypted = encrypt_memo(&sender, &recipient_pubkey, memo).unwrap();

        // Recipient can decrypt
        let decrypted = decrypt_memo(&recipient, &encrypted).unwrap();
        assert_eq!(decrypted, Some(memo.to_string()));

        // Sender can also decrypt (ECDH is commutative)
        let sender_decrypted = decrypt_memo(&sender, &encrypted).unwrap();
        assert_eq!(sender_decrypted, Some(memo.to_string()));
    }

    #[test]
    fn test_third_party_cannot_decrypt() {
        let sender = random_signing_key();
        let recipient = random_signing_key();
        let third_party = random_signing_key();
        let recipient_pubkey = recipient.verifying_key().to_bytes();

        let memo = "Secret payment note";
        let encrypted = encrypt_memo(&sender, &recipient_pubkey, memo).unwrap();

        // Third party cannot decrypt
        let result = decrypt_memo(&third_party, &encrypted).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_empty_memo_rejected() {
        let key = random_signing_key();
        let pubkey = key.verifying_key().to_bytes();
        assert!(matches!(
            encrypt_memo(&key, &pubkey, ""),
            Err(MemoError::Empty)
        ));
    }

    #[test]
    fn test_too_long_memo_rejected() {
        let key = random_signing_key();
        let pubkey = key.verifying_key().to_bytes();
        let long = "x".repeat(MEMO_MAX_LEN + 1);
        assert!(matches!(
            encrypt_memo(&key, &pubkey, &long),
            Err(MemoError::TooLong(_))
        ));
    }

    #[test]
    fn test_max_length_memo_ok() {
        let key = random_signing_key();
        let pubkey = key.verifying_key().to_bytes();
        let max_memo = "x".repeat(MEMO_MAX_LEN);
        let encrypted = encrypt_memo(&key, &pubkey, &max_memo).unwrap();
        let decrypted = decrypt_memo(&key, &encrypted).unwrap();
        assert_eq!(decrypted, Some(max_memo));
    }
}
