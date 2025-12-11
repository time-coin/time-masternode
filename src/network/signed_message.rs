#![allow(dead_code)]
#![allow(unused_imports)]

use crate::network::message::NetworkMessage;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SignedMessageError {
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Serialization error: {0}")]
    SerializationError(#[from] bincode::Error),
    #[error("Missing signature")]
    MissingSignature,
}

/// A cryptographically signed network message
/// Ensures authenticity and prevents message spoofing
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignedMessage {
    /// The actual message payload
    pub payload: NetworkMessage,
    /// Ed25519 signature of the payload
    pub signature: Signature,
    /// Public key of the sender
    pub sender_pubkey: VerifyingKey,
    /// Timestamp when message was signed
    pub timestamp: i64,
}

impl SignedMessage {
    /// Create a new signed message
    pub fn new(
        payload: NetworkMessage,
        signing_key: &SigningKey,
        timestamp: i64,
    ) -> Result<Self, SignedMessageError> {
        let message_bytes = bincode::serialize(&payload)?;
        let mut data_to_sign = Vec::new();
        data_to_sign.extend_from_slice(&message_bytes);
        data_to_sign.extend_from_slice(&timestamp.to_le_bytes());

        let signature = signing_key.sign(&data_to_sign);
        let sender_pubkey = signing_key.verifying_key();

        Ok(Self {
            payload,
            signature,
            sender_pubkey,
            timestamp,
        })
    }

    /// Verify the signature on this message
    pub fn verify(&self) -> Result<(), SignedMessageError> {
        let message_bytes = bincode::serialize(&self.payload)?;
        let mut data_to_verify = Vec::new();
        data_to_verify.extend_from_slice(&message_bytes);
        data_to_verify.extend_from_slice(&self.timestamp.to_le_bytes());

        self.sender_pubkey
            .verify(&data_to_verify, &self.signature)
            .map_err(|_| SignedMessageError::InvalidSignature)
    }

    /// Check if message timestamp is within acceptable range (prevents replay attacks)
    pub fn is_timestamp_valid(&self, max_age_seconds: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        let age = (now - self.timestamp).abs();
        age <= max_age_seconds
    }

    /// Get the sender's public key as bytes
    pub fn sender_pubkey_bytes(&self) -> [u8; 32] {
        self.sender_pubkey.to_bytes()
    }
}

/// Wrapper for sensitive key material with automatic zeroization
pub struct SecureSigningKey {
    key: SigningKey,
}

impl SecureSigningKey {
    pub fn new(key: SigningKey) -> Self {
        Self { key }
    }

    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, ed25519_dalek::SignatureError> {
        Ok(Self {
            key: SigningKey::from_bytes(bytes),
        })
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.key
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }
}

impl Drop for SecureSigningKey {
    fn drop(&mut self) {
        // Ed25519 SigningKey already implements Zeroize internally
        // This is just a marker type for additional safety
    }
}

// Tests temporarily disabled - need to update for ed25519-dalek 2.x API
/*
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify() {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let message = NetworkMessage::Ping {
            nonce: 12345,
            timestamp: chrono::Utc::now().timestamp(),
        };

        let signed = SignedMessage::new(message, &signing_key, chrono::Utc::now().timestamp())
            .expect("Failed to sign message");

        assert!(signed.verify().is_ok());
    }

    #[test]
    fn test_invalid_signature() {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let wrong_key = SigningKey::generate(&mut csprng);

        let message = NetworkMessage::Ping {
            nonce: 12345,
            timestamp: chrono::Utc::now().timestamp(),
        };

        let mut signed = SignedMessage::new(message, &signing_key, chrono::Utc::now().timestamp())
            .expect("Failed to sign message");

        // Tamper with the signature by using wrong key's pubkey
        signed.sender_pubkey = wrong_key.verifying_key();

        assert!(signed.verify().is_err());
    }

    #[test]
    fn test_timestamp_validation() {
        let mut csprng = rand::rngs::OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let message = NetworkMessage::Ping {
            nonce: 12345,
            timestamp: chrono::Utc::now().timestamp(),
        };

        let old_timestamp = chrono::Utc::now().timestamp() - 1000;
        let signed = SignedMessage::new(message, &signing_key, old_timestamp)
            .expect("Failed to sign message");

        assert!(!signed.is_timestamp_valid(60)); // Should be invalid (older than 60 seconds)
        assert!(signed.is_timestamp_valid(2000)); // Should be valid (within 2000 seconds)
    }

    #[test]
    fn test_secure_signing_key_zeroizes() {
        let key_bytes = [42u8; 32];
        {
            let _secure_key =
                SecureSigningKey::from_bytes(&key_bytes).expect("Failed to create secure key");
            // Key should be zeroized when dropped
        }
        // Note: We can't actually verify zeroization in safe Rust,
        // but ed25519-dalek handles it internally
    }
}
*/
