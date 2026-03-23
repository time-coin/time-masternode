//! Base58 encoding/decoding used for addresses and masternode keys.

const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Encode bytes to a base58 string (no checksum).
pub fn encode(data: &[u8]) -> String {
    let mut digits: Vec<u8> = Vec::new();

    for &byte in data {
        let mut carry = byte as u32;
        for d in digits.iter_mut() {
            carry += (*d as u32) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }

    let mut result = String::new();
    for &byte in data {
        if byte == 0 {
            result.push('1');
        } else {
            break;
        }
    }

    for &d in digits.iter().rev() {
        result.push(BASE58_ALPHABET[d as usize] as char);
    }

    result
}

/// Decode a base58 string to bytes. Returns an error on invalid characters.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let mut bytes: Vec<u8> = Vec::new();

    for ch in s.chars() {
        let idx = BASE58_ALPHABET
            .iter()
            .position(|&c| c == ch as u8)
            .ok_or_else(|| format!("Invalid base58 character: {}", ch))? as u32;

        let mut carry = idx;
        for b in bytes.iter_mut() {
            carry += (*b as u32) * 58;
            *b = (carry & 0xFF) as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }

    let leading_ones = s.chars().take_while(|&c| c == '1').count();
    let mut result = vec![0u8; leading_ones];
    result.extend(bytes.iter().rev());

    Ok(result)
}

/// Compute a 4-byte double-SHA256 checksum.
pub fn checksum(data: &[u8]) -> [u8; 4] {
    use sha2::{Digest, Sha256};
    let hash1 = Sha256::digest(data);
    let hash2 = Sha256::digest(hash1);
    let mut result = [0u8; 4];
    result.copy_from_slice(&hash2[..4]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let data = b"hello world";
        let encoded = encode(data);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_leading_zeros() {
        let data = vec![0, 0, 0, 1, 2, 3];
        let encoded = encode(&data);
        assert!(encoded.starts_with("111"));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_invalid_char() {
        assert!(decode("0OIl").is_err()); // 0, O, I, l not in base58
    }

    #[test]
    fn test_checksum() {
        let data = b"test";
        let cs = checksum(data);
        assert_eq!(cs.len(), 4);
        // Deterministic
        assert_eq!(cs, checksum(data));
    }
}
