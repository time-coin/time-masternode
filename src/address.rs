// src/address.rs
use crate::network_type::NetworkType;
use ed25519_dalek::VerifyingKey;
use sha2::{Digest, Sha256};
use std::fmt;

/// TIME Coin address format:
/// - Testnet: TIME0... (38 chars total)
/// - Mainnet: TIME1... (38 chars total)
///
/// Format: TIME{network_digit}{~33 base58 chars from 24 bytes}
/// The base58 encoding of 24 bytes (20 payload + 4 checksum) typically produces ~33 chars
#[allow(dead_code)]
const ADDRESS_LENGTH: usize = 38;
const BASE58_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    network: NetworkType,
    payload: [u8; 20], // RIPEMD-160 hash of public key
}

impl Address {
    /// Create address from public key
    pub fn from_public_key(pubkey: &VerifyingKey, network: NetworkType) -> Self {
        let pubkey_hash = Self::hash_public_key(pubkey);
        Self {
            network,
            payload: pubkey_hash,
        }
    }

    /// Create address from string (TIME0... or TIME1...)
    pub fn from_string(s: &str) -> Result<Self, AddressError> {
        // Base58 encoding can produce variable length, so check range instead of exact
        // Minimum: TIME + digit + ~30 chars = 35, Maximum: ~40
        if s.len() < 35 || s.len() > 45 {
            return Err(AddressError::InvalidLength);
        }

        if !s.starts_with("TIME") {
            return Err(AddressError::InvalidPrefix);
        }

        let network = match s.chars().nth(4) {
            Some('0') => NetworkType::Testnet,
            Some('1') => NetworkType::Mainnet,
            _ => return Err(AddressError::InvalidNetwork),
        };

        let encoded = &s[5..];
        let decoded = Self::decode_base58(encoded)?;

        if decoded.len() != 24 {
            return Err(AddressError::InvalidPayload);
        }

        // Verify checksum
        let payload_bytes = &decoded[..20];
        let checksum = &decoded[20..24];
        let computed_checksum = Self::compute_checksum(payload_bytes);

        if checksum != &computed_checksum[..4] {
            return Err(AddressError::InvalidChecksum);
        }

        let mut payload = [0u8; 20];
        payload.copy_from_slice(payload_bytes);

        Ok(Self { network, payload })
    }

    /// Convert address to string (TIME0... or TIME1...)
    pub fn as_string(&self) -> String {
        let network_digit = match self.network {
            NetworkType::Testnet => '0',
            NetworkType::Mainnet => '1',
        };

        let checksum = Self::compute_checksum(&self.payload);
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&self.payload);
        data.extend_from_slice(&checksum[..4]);

        let encoded = Self::encode_base58(&data);
        format!("TIME{}{}", network_digit, encoded)
    }

    #[allow(dead_code)]
    pub fn network(&self) -> NetworkType {
        self.network
    }

    #[allow(dead_code)]
    pub fn payload(&self) -> &[u8; 20] {
        &self.payload
    }

    /// Hash public key to create address payload (SHA256 -> RIPEMD160)
    fn hash_public_key(pubkey: &VerifyingKey) -> [u8; 20] {
        let sha_hash = Sha256::digest(pubkey.as_bytes());

        // For now, use first 20 bytes of SHA256 (RIPEMD160 not in std)
        let mut result = [0u8; 20];
        result.copy_from_slice(&sha_hash[..20]);
        result
    }

    fn compute_checksum(data: &[u8]) -> [u8; 4] {
        let hash1 = Sha256::digest(data);
        let hash2 = Sha256::digest(hash1);
        let mut checksum = [0u8; 4];
        checksum.copy_from_slice(&hash2[..4]);
        checksum
    }

    fn encode_base58(data: &[u8]) -> String {
        let mut num = num_bigint::BigUint::from_bytes_be(data);
        let base = num_bigint::BigUint::from(58u32);
        let mut result = String::new();

        while num > num_bigint::BigUint::from(0u32) {
            let remainder = &num % &base;
            num /= &base;
            let digits = remainder.to_u32_digits();
            let idx = if digits.is_empty() { 0 } else { digits[0] } as usize;
            result.insert(0, BASE58_ALPHABET[idx] as char);
        }

        // Add leading '1's for leading zeros
        for &byte in data {
            if byte == 0 {
                result.insert(0, '1');
            } else {
                break;
            }
        }

        result
    }

    fn decode_base58(s: &str) -> Result<Vec<u8>, AddressError> {
        let mut num = num_bigint::BigUint::from(0u32);
        let base = num_bigint::BigUint::from(58u32);

        for ch in s.chars() {
            let idx = BASE58_ALPHABET
                .iter()
                .position(|&c| c == ch as u8)
                .ok_or(AddressError::InvalidBase58)?;
            num = num * &base + idx;
        }

        let mut bytes = num.to_bytes_be();

        // Add leading zeros
        let leading_ones = s.chars().take_while(|&c| c == '1').count();
        let mut result = vec![0u8; leading_ones];
        result.append(&mut bytes);

        Ok(result)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_string())
    }
}

impl serde::Serialize for Address {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.as_string())
    }
}

impl<'de> serde::Deserialize<'de> for Address {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Address::from_string(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AddressError {
    #[error("Invalid address length")]
    InvalidLength,
    #[error("Invalid address prefix (expected TIME)")]
    InvalidPrefix,
    #[error("Invalid network digit (expected 0 or 1)")]
    InvalidNetwork,
    #[error("Invalid payload")]
    InvalidPayload,
    #[error("Invalid checksum")]
    InvalidChecksum,
    #[error("Invalid base58 character")]
    InvalidBase58,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn test_address_generation() {
        let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let verifying_key = signing_key.verifying_key();

        // Test testnet address
        let testnet_addr = Address::from_public_key(&verifying_key, NetworkType::Testnet);
        let testnet_str = testnet_addr.to_string();
        assert!(testnet_str.starts_with("TIME0"));
        // Base58 encoding produces variable length, check reasonable range
        assert!(testnet_str.len() >= 35 && testnet_str.len() <= 45);

        // Test mainnet address
        let mainnet_addr = Address::from_public_key(&verifying_key, NetworkType::Mainnet);
        let mainnet_str = mainnet_addr.to_string();
        assert!(mainnet_str.starts_with("TIME1"));
        assert!(mainnet_str.len() >= 35 && mainnet_str.len() <= 45);
    }

    #[test]
    fn test_address_round_trip() {
        let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let verifying_key = signing_key.verifying_key();

        let addr = Address::from_public_key(&verifying_key, NetworkType::Mainnet);
        let addr_str = addr.to_string();
        let parsed = Address::from_string(&addr_str).unwrap();

        assert_eq!(addr, parsed);
    }

    #[test]
    fn test_invalid_addresses() {
        assert!(Address::from_string("INVALID").is_err());
        assert!(Address::from_string("TIME2abcd").is_err());
        assert!(Address::from_string("TIME0").is_err());
    }
}
