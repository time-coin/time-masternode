//! Wallet management for key storage and transaction signing
//!
//! Security: Wallets are encrypted with AES-256-GCM using Argon2 key derivation

#![allow(dead_code)]

use crate::address::Address;
use crate::network_type::NetworkType;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::{
    password_hash::{PasswordHasher, SaltString},
    Argon2,
};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use zeroize::Zeroize;

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("Failed to create wallet: {0}")]
    CreationFailed(String),
    #[error("Failed to load wallet: {0}")]
    LoadFailed(String),
    #[error("Failed to save wallet: {0}")]
    SaveFailed(String),
    #[error("Wallet file not found")]
    NotFound,
    #[error("Invalid password")]
    InvalidPassword,
    #[error("Encryption error: {0}")]
    EncryptionError(String),
}

/// Encrypted wallet file format
#[derive(Serialize, Deserialize)]
struct EncryptedWalletFile {
    /// File format version
    version: u32,
    /// Argon2 salt for key derivation
    salt: String,
    /// AES-GCM nonce (12 bytes)
    nonce: Vec<u8>,
    /// Encrypted wallet data
    ciphertext: Vec<u8>,
}

/// Bitcoin-style wallet storage format (plaintext, for encryption)
#[derive(Serialize, Deserialize, Clone)]
pub struct WalletData {
    /// Wallet version (for future upgrades)
    pub version: u32,
    /// Network type (testnet/mainnet)
    pub network: NetworkType,
    /// Master keypair
    pub keypair: KeypairData,
    /// TIME Coin address
    pub address: String,
    /// Creation timestamp
    pub created_at: i64,
    /// Wallet label/name
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct KeypairData {
    /// 32-byte secret key
    pub secret_key: [u8; 32],
    /// 32-byte public key
    pub public_key: [u8; 32],
}

pub struct Wallet {
    data: WalletData,
    #[allow(dead_code)]
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl Wallet {
    /// Create a new wallet
    pub fn new(network: NetworkType, label: Option<String>) -> Result<Self, WalletError> {
        let _csprng = OsRng;
        let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let verifying_key = signing_key.verifying_key();

        let address = Address::from_public_key(verifying_key.as_bytes(), network);

        let data = WalletData {
            version: 1,
            network,
            keypair: KeypairData {
                secret_key: signing_key.to_bytes(),
                public_key: verifying_key.to_bytes(),
            },
            address: address.to_string(),
            created_at: chrono::Utc::now().timestamp(),
            label,
        };

        Ok(Wallet {
            data,
            signing_key,
            verifying_key,
        })
    }

    /// Load wallet from encrypted file
    pub fn load<P: AsRef<Path>>(path: P, password: &str) -> Result<Self, WalletError> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(WalletError::NotFound);
        }

        let contents = fs::read(path)
            .map_err(|e| WalletError::LoadFailed(format!("Failed to read file: {}", e)))?;

        // Try to deserialize as encrypted file (new format)
        let encrypted_file: EncryptedWalletFile = match bincode::deserialize(&contents) {
            Ok(ef) => ef,
            Err(_) => {
                // Fall back to old unencrypted format and migrate
                tracing::warn!("⚠️  Old wallet format detected - migrating to encrypted format");
                let old_data: WalletData = bincode::deserialize(&contents).map_err(|e| {
                    WalletError::LoadFailed(format!("Failed to deserialize old format: {}", e))
                })?;

                // Reconstruct wallet from old data
                let signing_key = SigningKey::from_bytes(&old_data.keypair.secret_key);
                let verifying_key = VerifyingKey::from_bytes(&old_data.keypair.public_key)
                    .map_err(|e| WalletError::LoadFailed(format!("Invalid public key: {}", e)))?;

                let wallet = Wallet {
                    data: old_data,
                    signing_key,
                    verifying_key,
                };

                // Save in new encrypted format
                wallet.save(path, password)?;
                tracing::info!("✓ Wallet migrated to encrypted format");

                return Ok(wallet);
            }
        };

        // Derive decryption key from password
        let mut key = Self::derive_key(password, &encrypted_file.salt)?;

        // Decrypt wallet data
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| WalletError::EncryptionError(e.to_string()))?;

        let nonce = Nonce::from_slice(&encrypted_file.nonce);

        let plaintext = cipher
            .decrypt(nonce, encrypted_file.ciphertext.as_ref())
            .map_err(|_| WalletError::InvalidPassword)?;

        // Zeroize key material
        key.zeroize();

        // Deserialize wallet data
        let data: WalletData = bincode::deserialize(&plaintext)
            .map_err(|e| WalletError::LoadFailed(format!("Failed to deserialize: {}", e)))?;

        // Reconstruct keypair
        let signing_key = SigningKey::from_bytes(&data.keypair.secret_key);
        let verifying_key = VerifyingKey::from_bytes(&data.keypair.public_key)
            .map_err(|e| WalletError::LoadFailed(format!("Invalid public key: {}", e)))?;

        Ok(Wallet {
            data,
            signing_key,
            verifying_key,
        })
    }

    /// Save wallet to encrypted file
    pub fn save<P: AsRef<Path>>(&self, path: P, password: &str) -> Result<(), WalletError> {
        let path = path.as_ref();

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                WalletError::SaveFailed(format!("Failed to create directory: {}", e))
            })?;
        }

        // Generate random salt for Argon2
        let salt = SaltString::generate(&mut OsRng);

        // Derive encryption key from password
        let mut key = Self::derive_key(password, salt.as_str())?;

        // Encrypt wallet data
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|e| WalletError::EncryptionError(e.to_string()))?;

        // Generate random nonce (12 bytes for AES-GCM)
        let nonce_bytes: [u8; 12] = rand::random();
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Serialize wallet data
        let plaintext = bincode::serialize(&self.data)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to serialize: {}", e)))?;

        // Encrypt
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|e| WalletError::EncryptionError(e.to_string()))?;

        // Zeroize key material
        key.zeroize();

        // Create encrypted file structure
        let encrypted_file = EncryptedWalletFile {
            version: 1,
            salt: salt.to_string(),
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        };

        // Serialize encrypted file
        let contents = bincode::serialize(&encrypted_file)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to serialize: {}", e)))?;

        // Write atomically
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &contents)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to write temp file: {}", e)))?;

        fs::rename(&temp_path, path)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to rename: {}", e)))?;

        Ok(())
    }

    /// Derive encryption key from password using Argon2
    fn derive_key(password: &str, salt_str: &str) -> Result<[u8; 32], WalletError> {
        let argon2 = Argon2::default();

        let salt = SaltString::from_b64(salt_str)
            .map_err(|e| WalletError::EncryptionError(format!("Invalid salt: {}", e)))?;

        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| WalletError::EncryptionError(format!("Key derivation failed: {}", e)))?;

        // Extract 32-byte key from hash
        let hash_bytes = password_hash
            .hash
            .ok_or_else(|| WalletError::EncryptionError("No hash output".to_string()))?;

        let mut key = [0u8; 32];
        let hash_slice = hash_bytes.as_bytes();
        key.copy_from_slice(&hash_slice[..32.min(hash_slice.len())]);

        Ok(key)
    }

    /// Get wallet address
    pub fn address(&self) -> &str {
        &self.data.address
    }

    /// Get public key
    pub fn public_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Get signing key (for signing transactions)
    #[allow(dead_code)]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Get network type
    #[allow(dead_code)]
    pub fn network(&self) -> NetworkType {
        self.data.network
    }

    /// Get wallet info
    #[allow(dead_code)]
    pub fn info(&self) -> WalletInfo {
        WalletInfo {
            version: self.data.version,
            network: self.data.network,
            address: self.data.address.clone(),
            created_at: self.data.created_at,
            label: self.data.label.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct WalletInfo {
    pub version: u32,
    pub network: NetworkType,
    pub address: String,
    pub created_at: i64,
    pub label: Option<String>,
}

/// Wallet manager for handling multiple wallets
pub struct WalletManager {
    data_dir: String,
}

impl WalletManager {
    pub fn new(data_dir: String) -> Self {
        Self { data_dir }
    }

    /// Get default wallet path
    pub fn default_wallet_path(&self) -> String {
        format!("{}/time-wallet.dat", self.data_dir)
    }

    /// Create or load wallet
    /// NOTE: Uses default password "timecoin" for development.
    /// TODO: In production, prompt user for password
    pub fn get_or_create_wallet(&self, network: NetworkType) -> Result<Wallet, WalletError> {
        let path = self.default_wallet_path();
        const DEFAULT_PASSWORD: &str = "timecoin";

        if Path::new(&path).exists() {
            Wallet::load(&path, DEFAULT_PASSWORD)
        } else {
            let wallet = Wallet::new(network, Some("Default Wallet".to_string()))?;
            wallet.save(&path, DEFAULT_PASSWORD)?;
            Ok(wallet)
        }
    }

    /// Create new wallet
    #[allow(dead_code)]
    pub fn create_wallet(
        &self,
        network: NetworkType,
        label: Option<String>,
        password: &str,
    ) -> Result<Wallet, WalletError> {
        let wallet = Wallet::new(network, label)?;
        wallet.save(self.default_wallet_path(), password)?;
        Ok(wallet)
    }

    /// Load existing wallet
    #[allow(dead_code)]
    pub fn load_wallet(&self, password: &str) -> Result<Wallet, WalletError> {
        Wallet::load(self.default_wallet_path(), password)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_wallet_creation() {
        let wallet = Wallet::new(NetworkType::Testnet, Some("Test".to_string())).unwrap();
        assert!(wallet.address().starts_with("TIME0"));
    }

    #[test]
    fn test_wallet_save_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-wallet.dat");

        // Create and save
        let wallet = Wallet::new(NetworkType::Mainnet, None).unwrap();
        let original_address = wallet.address().to_string();
        wallet.save(&path, "test_password").unwrap();

        // Load and verify
        let loaded = Wallet::load(&path, "test_password").unwrap();
        assert_eq!(loaded.address(), original_address);
        assert_eq!(loaded.network(), NetworkType::Mainnet);
    }
}
