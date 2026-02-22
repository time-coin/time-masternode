//! Masternode key encoding/decoding utilities.
//!
//! Masternodes use a single Ed25519 private key for identity and consensus signing.
//! Generate one with `time-cli masternodegenkey` and add it to time.conf as
//! `masternodeprivkey=<base58check-encoded key>`.

use sha2::{Digest, Sha256};

const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

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
    fn test_base58_roundtrip() {
        let data = vec![0x80, 1, 2, 3, 4, 5];
        let encoded = encode_base58(&data);
        let decoded = decode_base58(&encoded).unwrap();
        assert_eq!(data, decoded);
    }
}
