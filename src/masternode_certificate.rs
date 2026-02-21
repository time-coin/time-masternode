//! Masternode certificate verification for website-issued keys.
//!
//! Masternodes must register on the TIME Coin website with an email address
//! to receive an Ed25519 keypair. The website signs the masternode's public key
//! with its master authority key, producing a certificate. The daemon verifies
//! this certificate on-chain before allowing masternode registration.
//!
//! Security: Ed25519 signatures (128-bit security) — computationally infeasible
//! to forge without the website's master private key.

use crate::constants::masternode_authority::{ENFORCE_CERTIFICATE, MASTERNODE_AUTHORITY_PUBKEY};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Verify that a masternode public key was signed by the TIME Coin authority.
///
/// # Arguments
/// * `masternode_pubkey` - The masternode's Ed25519 public key (32 bytes)
/// * `certificate` - The authority's signature over the masternode pubkey (64 bytes)
///
/// # Returns
/// * `true` if the certificate is valid (signed by authority)
/// * `true` if certificate enforcement is disabled (testnet development)
/// * `false` if the certificate is invalid or authority key is not configured
pub fn verify_masternode_certificate(masternode_pubkey: &VerifyingKey, certificate: &[u8]) -> bool {
    if !ENFORCE_CERTIFICATE {
        tracing::debug!(
            "🔓 Certificate enforcement disabled — accepting masternode without verification"
        );
        return true;
    }

    // Certificate must be exactly 64 bytes (Ed25519 signature)
    let cert_bytes: &[u8; 64] = match certificate.try_into() {
        Ok(b) => b,
        Err(_) => {
            tracing::warn!(
                "❌ Invalid certificate length: expected 64 bytes, got {}",
                certificate.len()
            );
            return false;
        }
    };

    // Check if authority key is configured (not all zeros)
    if MASTERNODE_AUTHORITY_PUBKEY == [0u8; 32] {
        tracing::warn!(
            "⚠️ Masternode authority public key not configured — cannot verify certificates"
        );
        return false;
    }

    // Load the authority public key
    let authority_key = match VerifyingKey::from_bytes(&MASTERNODE_AUTHORITY_PUBKEY) {
        Ok(key) => key,
        Err(e) => {
            tracing::error!("❌ Invalid authority public key in constants: {}", e);
            return false;
        }
    };

    // Parse the certificate as an Ed25519 signature
    let signature = Signature::from_bytes(cert_bytes);

    // Verify: authority_key signed the masternode's public key bytes
    match authority_key.verify(masternode_pubkey.as_bytes(), &signature) {
        Ok(()) => {
            tracing::info!(
                "✅ Masternode certificate verified for pubkey {}",
                hex::encode(masternode_pubkey.as_bytes())
            );
            true
        }
        Err(_) => {
            tracing::warn!(
                "❌ Invalid masternode certificate for pubkey {} — not signed by authority",
                hex::encode(masternode_pubkey.as_bytes())
            );
            false
        }
    }
}

/// Decode a base58check-encoded masternode private key.
///
/// Format: 1-byte prefix (0x80) + 32-byte Ed25519 secret key + 4-byte checksum
/// Returns the 32-byte secret key bytes on success.
pub fn decode_masternode_key(base58_key: &str) -> Result<[u8; 32], String> {
    let bytes = decode_base58(base58_key).map_err(|e| format!("Invalid base58: {}", e))?;

    // Expected: 1 (prefix) + 32 (key) + 4 (checksum) = 37 bytes
    if bytes.len() != 37 {
        return Err(format!(
            "Invalid masternode key length: expected 37 bytes, got {}",
            bytes.len()
        ));
    }

    // Check prefix
    if bytes[0] != 0x80 {
        return Err(format!(
            "Invalid masternode key prefix: expected 0x80, got 0x{:02x}",
            bytes[0]
        ));
    }

    // Verify checksum (first 4 bytes of double-SHA256)
    let payload = &bytes[..33]; // prefix + key
    let hash1 = Sha256::digest(payload);
    let hash2 = Sha256::digest(hash1);
    let checksum = &hash2[..4];

    if &bytes[33..37] != checksum {
        return Err("Invalid masternode key checksum".to_string());
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes[1..33]);
    Ok(key)
}

/// Encode a 32-byte Ed25519 secret key as base58check.
///
/// Format: 0x80 prefix + 32-byte key + 4-byte double-SHA256 checksum
pub fn encode_masternode_key(secret_key: &[u8; 32]) -> String {
    let mut payload = Vec::with_capacity(37);
    payload.push(0x80); // prefix
    payload.extend_from_slice(secret_key);

    // Checksum: first 4 bytes of double-SHA256
    let hash1 = Sha256::digest(&payload);
    let hash2 = Sha256::digest(hash1);
    payload.extend_from_slice(&hash2[..4]);

    encode_base58(&payload)
}

/// Decode a hex-encoded certificate (64-byte Ed25519 signature).
pub fn decode_certificate(hex_cert: &str) -> Result<[u8; 64], String> {
    let bytes = hex::decode(hex_cert).map_err(|e| format!("Invalid hex certificate: {}", e))?;

    if bytes.len() != 64 {
        return Err(format!(
            "Invalid certificate length: expected 64 bytes, got {}",
            bytes.len()
        ));
    }

    let mut cert = [0u8; 64];
    cert.copy_from_slice(&bytes);
    Ok(cert)
}

fn encode_base58(data: &[u8]) -> String {
    let mut num = num_bigint::BigUint::from_bytes_be(data);
    let base = num_bigint::BigUint::from(58u32);
    let zero = num_bigint::BigUint::from(0u32);
    let mut result = String::new();

    while num > zero {
        let remainder = &num % &base;
        num /= &base;
        let digits = remainder.to_u32_digits();
        let idx = if digits.is_empty() { 0 } else { digits[0] } as usize;
        result.insert(0, BASE58_ALPHABET[idx] as char);
    }

    for &byte in data {
        if byte == 0 {
            result.insert(0, '1');
        } else {
            break;
        }
    }

    result
}

fn decode_base58(s: &str) -> Result<Vec<u8>, String> {
    let mut num = num_bigint::BigUint::from(0u32);
    let base = num_bigint::BigUint::from(58u32);

    for ch in s.chars() {
        let idx = BASE58_ALPHABET
            .iter()
            .position(|&c| c == ch as u8)
            .ok_or_else(|| format!("Invalid base58 character: {}", ch))?;
        num = num * &base + idx;
    }

    let mut bytes = num.to_bytes_be();

    let leading_ones = s.chars().take_while(|&c| c == '1').count();
    let mut result = vec![0u8; leading_ones];
    result.append(&mut bytes);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn test_key_encode_decode_roundtrip() {
        let secret = [42u8; 32];
        let encoded = encode_masternode_key(&secret);
        let decoded = decode_masternode_key(&encoded).unwrap();
        assert_eq!(secret, decoded);
    }

    #[test]
    fn test_decode_invalid_base58() {
        assert!(decode_masternode_key("not-valid-base58!!!").is_err());
    }

    #[test]
    fn test_decode_wrong_length() {
        let encoded = encode_base58(&[0x80, 1, 2, 3]);
        assert!(decode_masternode_key(&encoded).is_err());
    }

    #[test]
    fn test_decode_bad_checksum() {
        let mut payload = vec![0x80];
        payload.extend_from_slice(&[42u8; 32]);
        payload.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // bad checksum
        let encoded = encode_base58(&payload);
        assert!(decode_masternode_key(&encoded).is_err());
    }

    #[test]
    fn test_certificate_decode() {
        let cert_bytes = [0xABu8; 64];
        let hex_cert = hex::encode(cert_bytes);
        let decoded = decode_certificate(&hex_cert).unwrap();
        assert_eq!(cert_bytes, decoded);
    }

    #[test]
    fn test_certificate_decode_wrong_length() {
        let hex_cert = hex::encode([0u8; 32]); // 32 bytes, not 64
        assert!(decode_certificate(&hex_cert).is_err());
    }

    #[test]
    fn test_verify_certificate_enforcement_disabled() {
        // With ENFORCE_CERTIFICATE = false, any certificate should pass
        let signing_key = SigningKey::from_bytes(&[1u8; 32]);
        let pubkey = signing_key.verifying_key();
        let fake_cert = [0u8; 64];
        assert!(verify_masternode_certificate(&pubkey, &fake_cert));
    }

    #[test]
    fn test_base58_roundtrip() {
        let data = vec![0x80, 1, 2, 3, 4, 5];
        let encoded = encode_base58(&data);
        let decoded = decode_base58(&encoded).unwrap();
        assert_eq!(data, decoded);
    }
}
