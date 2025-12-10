use crate::types::{Masternode, MasternodeTier};
use crate::NetworkType;
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Masternode already registered")]
    AlreadyRegistered,
    #[error("Masternode not found")]
    NotFound,
    #[error("Invalid collateral amount")]
    InvalidCollateral,
    #[error("Storage error: {0}")]
    Storage(String),
}

pub struct MasternodeRegistry {
    masternodes: Arc<RwLock<HashMap<String, Masternode>>>,
    db: Arc<Db>,
    network: NetworkType,
}

impl MasternodeRegistry {
    pub fn new(db: Arc<Db>, network: NetworkType) -> Self {
        // Load existing masternodes from disk
        let mut nodes: HashMap<String, Masternode> = HashMap::new();
        for result in db.scan_prefix(b"masternode:") {
            if let Ok((key, value)) = result {
                if let Ok(masternode) = bincode::deserialize::<Masternode>(&value) {
                    let addr = String::from_utf8_lossy(&key[11..]).to_string();
                    nodes.insert(addr, masternode);
                }
            }
        }

        if !nodes.is_empty() {
            tracing::info!("✓ Loaded {} masternodes from registry", nodes.len());
        }

        Self {
            masternodes: Arc::new(RwLock::new(nodes)),
            db,
            network,
        }
    }

    pub async fn register(&self, masternode: Masternode) -> Result<(), RegistryError> {
        // Validate collateral
        let required = match masternode.tier {
            MasternodeTier::Free => 0,
            MasternodeTier::Bronze => 1_000,
            MasternodeTier::Silver => 10_000,
            MasternodeTier::Gold => 100_000,
        };

        if masternode.collateral < required {
            return Err(RegistryError::InvalidCollateral);
        }

        let mut nodes = self.masternodes.write().await;

        if nodes.contains_key(&masternode.address) {
            return Err(RegistryError::AlreadyRegistered);
        }

        // Persist to disk
        let key = format!("masternode:{}", masternode.address);
        let value =
            bincode::serialize(&masternode).map_err(|e| RegistryError::Storage(e.to_string()))?;

        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| RegistryError::Storage(e.to_string()))?;

        nodes.insert(masternode.address.clone(), masternode);
        Ok(())
    }

    pub async fn unregister(&self, address: &str) -> Result<(), RegistryError> {
        let mut nodes = self.masternodes.write().await;

        if !nodes.contains_key(address) {
            return Err(RegistryError::NotFound);
        }

        // Remove from disk
        let key = format!("masternode:{}", address);
        self.db
            .remove(key.as_bytes())
            .map_err(|e| RegistryError::Storage(e.to_string()))?;

        nodes.remove(address);
        Ok(())
    }

    pub async fn get(&self, address: &str) -> Option<Masternode> {
        self.masternodes.read().await.get(address).cloned()
    }

    pub async fn list_all(&self) -> Vec<Masternode> {
        self.masternodes.read().await.values().cloned().collect()
    }

    pub async fn list_by_tier(&self, tier: MasternodeTier) -> Vec<Masternode> {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|mn| std::mem::discriminant(&mn.tier) == std::mem::discriminant(&tier))
            .cloned()
            .collect()
    }

    pub async fn count(&self) -> usize {
        self.masternodes.read().await.len()
    }

    pub async fn is_registered(&self, address: &str) -> bool {
        self.masternodes.read().await.contains_key(address)
    }
}
