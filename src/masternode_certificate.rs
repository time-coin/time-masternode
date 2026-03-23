//! Masternode key encoding/decoding utilities.
//!
//! Masternodes use a single Ed25519 private key for identity and consensus signing.
//! Generate one with `time-cli masternodegenkey` and add it to time.conf as
//! `masternodeprivkey=<base58check-encoded key>`.

use crate::crypto::base58;

/// Decode a base58check-encoded masternode private key.
///
/// Format: 1-byte prefix (0x80) + 32-byte Ed25519 secret key + 4-byte checksum
/// Returns the 32-byte secret key bytes on success.
pub fn decode_masternode_key(base58_key: &str) -> Result<[u8; 32], String> {
    let bytes = base58::decode(base58_key).map_err(|e| format!("Invalid base58: {}", e))?;

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
    let checksum = base58::checksum(payload);

    if bytes[33..37] != checksum {
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
    let cs = base58::checksum(&payload);
    payload.extend_from_slice(&cs);

    base58::encode(&payload)
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
        let encoded = base58::encode(&[0x80, 1, 2, 3]);
        assert!(decode_masternode_key(&encoded).is_err());
    }

    #[test]
    fn test_decode_bad_checksum() {
        let mut payload = vec![0x80];
        payload.extend_from_slice(&[42u8; 32]);
        payload.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]); // bad checksum
        let encoded = base58::encode(&payload);
        assert!(decode_masternode_key(&encoded).is_err());
    }

    #[test]
    fn test_base58_roundtrip() {
        let data = vec![0x80, 1, 2, 3, 4, 5];
        let encoded = base58::encode(&data);
        let decoded = base58::decode(&encoded).unwrap();
        assert_eq!(data, decoded);
    }
}
