use crate::address::Address;
use crate::network_type::NetworkType;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("Failed to create wallet: {0}")]
    #[allow(dead_code)]
    CreationFailed(String),
    #[error("Failed to load wallet: {0}")]
    LoadFailed(String),
    #[error("Failed to save wallet: {0}")]
    SaveFailed(String),
    #[error("Wallet file not found")]
    NotFound,
}

/// Bitcoin-style wallet storage format
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
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
}

impl Wallet {
    /// Create a new wallet
    pub fn new(network: NetworkType, label: Option<String>) -> Result<Self, WalletError> {
        let _csprng = OsRng;
        let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let verifying_key = signing_key.verifying_key();

        let address = Address::from_public_key(&verifying_key, network);

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

    /// Load wallet from file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, WalletError> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(WalletError::NotFound);
        }

        let contents = fs::read(path)
            .map_err(|e| WalletError::LoadFailed(format!("Failed to read file: {}", e)))?;

        // Decrypt/deserialize (simplified - in production use proper encryption)
        let data: WalletData = bincode::deserialize(&contents)
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

    /// Save wallet to file (Bitcoin-style format)
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), WalletError> {
        let path = path.as_ref();

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                WalletError::SaveFailed(format!("Failed to create directory: {}", e))
            })?;
        }

        // Serialize wallet data (in production: encrypt with AES-256)
        let contents = bincode::serialize(&self.data)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to serialize: {}", e)))?;

        // Write atomically
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, &contents)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to write temp file: {}", e)))?;

        fs::rename(&temp_path, path)
            .map_err(|e| WalletError::SaveFailed(format!("Failed to rename: {}", e)))?;

        Ok(())
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
    pub fn get_or_create_wallet(&self, network: NetworkType) -> Result<Wallet, WalletError> {
        let path = self.default_wallet_path();

        if Path::new(&path).exists() {
            Wallet::load(&path)
        } else {
            let wallet = Wallet::new(network, Some("Default Wallet".to_string()))?;
            wallet.save(&path)?;
            Ok(wallet)
        }
    }

    /// Create new wallet
    #[allow(dead_code)]
    pub fn create_wallet(
        &self,
        network: NetworkType,
        label: Option<String>,
    ) -> Result<Wallet, WalletError> {
        let wallet = Wallet::new(network, label)?;
        wallet.save(self.default_wallet_path())?;
        Ok(wallet)
    }

    /// Load existing wallet
    #[allow(dead_code)]
    pub fn load_wallet(&self) -> Result<Wallet, WalletError> {
        Wallet::load(self.default_wallet_path())
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
        wallet.save(&path).unwrap();

        // Load and verify
        let loaded = Wallet::load(&path).unwrap();
        assert_eq!(loaded.address(), original_address);
        assert_eq!(loaded.network(), NetworkType::Mainnet);
    }
}
