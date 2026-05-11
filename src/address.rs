// src/address.rs
use crate::crypto::base58;
use crate::network_type::NetworkType;
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    network: NetworkType,
    payload: [u8; 20], // RIPEMD-160 hash of public key
}

impl Address {
    /// Create address from a public key (Ed25519: 32 bytes)
    pub fn from_public_key(pubkey: &[u8], network: NetworkType) -> Self {
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
    fn hash_public_key(pubkey: &[u8]) -> [u8; 20] {
        let sha_hash = Sha256::digest(pubkey);

        // For now, use first 20 bytes of SHA256 (RIPEMD160 not in std)
        let mut result = [0u8; 20];
        result.copy_from_slice(&sha_hash[..20]);
        result
    }

    fn compute_checksum(data: &[u8]) -> [u8; 4] {
        base58::checksum(data)
    }

    fn encode_base58(data: &[u8]) -> String {
        base58::encode(data)
    }

    fn decode_base58(s: &str) -> Result<Vec<u8>, AddressError> {
        base58::decode(s).map_err(|_| AddressError::InvalidBase58)
    }
}

/// Verify a collateral-claim proof.
///
/// A valid proof requires *all three*:
///   1. **Reward routing rule**: `reward_address == utxo_address`.  The
///      announcer must direct rewards to the address that owns the
///      collateral UTXO.  This is the economic anti-hijack mechanism — a
///      squatter announcing with their own reward address gains nothing
///      because rewards flow to the legitimate owner anyway, and an
///      announce that violates this rule cannot win conflict resolution.
///   2. **Key control**: the announcer's `public_key` derives to
///      `utxo_address` (SHA-256 → 20 bytes → base58check with network
///      prefix; see `Address::from_public_key`).  This proves the
///      announcer controls the private key for the address that owns
///      the collateral UTXO.
///   3. **Signature**: `signature` is a valid Ed25519 signature by
///      `public_key` over the canonical claim message
///      `"TIME_COLLATERAL_CLAIM:<txid_hex>:<vout>"`.
///
/// Returns true iff all three checks pass.
/// Verify a V4 collateral ownership proof.
///
/// Two wire formats are supported:
///
/// - **64-byte proof** (masternodeprivkey owns UTXO): the 64-byte bytes are an Ed25519
///   signature over `"TIME_COLLATERAL_CLAIM:{txid_hex}:{vout}"` by `public_key`.
///   Valid when `public_key` derives to `utxo_address`.
///
/// - **96-byte proof** (wallet key owns UTXO): first 32 bytes are an Ed25519 public key
///   (`wallet_pubkey`), remaining 64 bytes are a signature over the same message by
///   `wallet_pubkey`.  Valid when `wallet_pubkey` derives to `utxo_address`.
///   This is the common case where collateral sits at the wallet address rather than at
///   an address derived from the masternodeprivkey.
pub fn verify_collateral_claim_proof(
    public_key: &ed25519_dalek::VerifyingKey,
    proof: &[u8],
    reward_address: &str,
    utxo_address: &str,
    outpoint_txid: &[u8; 32],
    outpoint_vout: u32,
) -> bool {
    if proof.is_empty() || utxo_address.is_empty() {
        return false;
    }
    if reward_address != utxo_address {
        return false;
    }
    let network = if utxo_address.starts_with("TIME0") {
        crate::network_type::NetworkType::Testnet
    } else if utxo_address.starts_with("TIME1") {
        crate::network_type::NetworkType::Mainnet
    } else {
        return false;
    };
    let proof_msg = format!(
        "TIME_COLLATERAL_CLAIM:{}:{}",
        hex::encode(outpoint_txid),
        outpoint_vout
    );
    use ed25519_dalek::Verifier;
    match proof.len() {
        64 => {
            // 64-byte format: proof signed by the announcement's identity key (masternodeprivkey).
            let derived = Address::from_public_key(public_key.as_bytes(), network).as_string();
            if derived != utxo_address {
                return false;
            }
            ed25519_dalek::Signature::from_slice(proof)
                .map(|sig| public_key.verify(proof_msg.as_bytes(), &sig).is_ok())
                .unwrap_or(false)
        }
        96 => {
            // 96-byte format: [wallet_pubkey (32)] + [wallet_signature (64)].
            // The wallet key owns the UTXO; the masternodeprivkey is only the node identity.
            let pk_bytes: [u8; 32] = proof[..32].try_into().unwrap_or([0u8; 32]);
            let Ok(wallet_pk) = ed25519_dalek::VerifyingKey::from_bytes(&pk_bytes) else {
                return false;
            };
            let derived = Address::from_public_key(wallet_pk.as_bytes(), network).as_string();
            if derived != utxo_address {
                return false;
            }
            ed25519_dalek::Signature::from_slice(&proof[32..])
                .map(|sig| wallet_pk.verify(proof_msg.as_bytes(), &sig).is_ok())
                .unwrap_or(false)
        }
        _ => false,
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
        let pubkey_bytes = signing_key.verifying_key().to_bytes();

        // Test testnet address
        let testnet_addr = Address::from_public_key(&pubkey_bytes, NetworkType::Testnet);
        let testnet_str = testnet_addr.to_string();
        assert!(testnet_str.starts_with("TIME0"));
        assert!(testnet_str.len() >= 35 && testnet_str.len() <= 45);

        // Test mainnet address
        let mainnet_addr = Address::from_public_key(&pubkey_bytes, NetworkType::Mainnet);
        let mainnet_str = mainnet_addr.to_string();
        assert!(mainnet_str.starts_with("TIME1"));
        assert!(mainnet_str.len() >= 35 && mainnet_str.len() <= 45);
    }

    #[test]
    fn test_address_round_trip() {
        let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let pubkey_bytes = signing_key.verifying_key().to_bytes();

        let addr = Address::from_public_key(&pubkey_bytes, NetworkType::Mainnet);
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
