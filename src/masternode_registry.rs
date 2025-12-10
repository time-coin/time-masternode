use crate::types::{Masternode, MasternodeTier};
use crate::NetworkType;
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{info, warn};

const HEARTBEAT_INTERVAL_SECS: u64 = 60; // Masternodes must ping every 60 seconds
const MAX_MISSED_HEARTBEATS: u64 = 3; // Allow 3 missed heartbeats before marking offline

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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MasternodeInfo {
    pub masternode: Masternode,
    pub reward_address: String, // Address to send block rewards
    pub last_heartbeat: u64,
    pub uptime_start: u64, // When current uptime period started
    pub total_uptime: u64, // Total uptime in seconds
    pub is_active: bool,
}

pub struct MasternodeRegistry {
    masternodes: Arc<RwLock<HashMap<String, MasternodeInfo>>>,
    db: Arc<Db>,
    network: NetworkType,
    block_period_start: Arc<RwLock<u64>>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
}

impl MasternodeRegistry {
    pub fn new(db: Arc<Db>, network: NetworkType) -> Self {
        let now = Self::now();
        // Load existing masternodes from disk
        let mut nodes: HashMap<String, MasternodeInfo> = HashMap::new();
        for (key, value) in db.scan_prefix(b"masternode:").flatten() {
            if let Ok(info) = bincode::deserialize::<MasternodeInfo>(&value) {
                let addr = String::from_utf8_lossy(&key[11..]).to_string();
                nodes.insert(addr, info);
            }
        }

        if !nodes.is_empty() {
            tracing::info!("✓ Loaded {} masternodes from registry", nodes.len());
        }

        let registry = Self {
            masternodes: Arc::new(RwLock::new(nodes)),
            db,
            network,
            block_period_start: Arc::new(RwLock::new(now)),
            peer_manager: Arc::new(RwLock::new(None)),
        };

        // Start heartbeat monitor
        tokio::spawn({
            let registry = registry.clone();
            async move {
                registry.monitor_heartbeats().await;
            }
        });

        registry
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    async fn monitor_heartbeats(&self) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;

            let now = Self::now();
            let mut masternodes = self.masternodes.write().await;

            for (address, info) in masternodes.iter_mut() {
                if info.is_active {
                    let time_since_heartbeat = now - info.last_heartbeat;
                    let max_silence = HEARTBEAT_INTERVAL_SECS * MAX_MISSED_HEARTBEATS;

                    if time_since_heartbeat > max_silence {
                        // Mark as offline
                        info.is_active = false;
                        if info.uptime_start > 0 {
                            info.total_uptime += now - info.uptime_start;
                        }
                        warn!(
                            "⚠️  Masternode {} marked offline (no heartbeat for {}s)",
                            address, time_since_heartbeat
                        );

                        // Persist to disk
                        let key = format!("masternode:{}", address);
                        if let Ok(value) = bincode::serialize(&info) {
                            let _ = self.db.insert(key.as_bytes(), value);
                        }
                    }
                }
            }
        }
    }

    pub async fn register(
        &self,
        masternode: Masternode,
        reward_address: String,
    ) -> Result<(), RegistryError> {
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

        let now = Self::now();
        let info = MasternodeInfo {
            masternode: masternode.clone(),
            reward_address,
            last_heartbeat: now,
            uptime_start: now,
            total_uptime: 0,
            is_active: true,
        };

        // Persist to disk
        let key = format!("masternode:{}", masternode.address);
        let value = bincode::serialize(&info).map_err(|e| RegistryError::Storage(e.to_string()))?;

        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| RegistryError::Storage(e.to_string()))?;

        nodes.insert(masternode.address.clone(), info);
        info!("✓ Registered masternode: {}", masternode.address);
        Ok(())
    }

    pub async fn heartbeat(&self, address: &str) -> Result<(), RegistryError> {
        let now = Self::now();
        let mut masternodes = self.masternodes.write().await;

        if let Some(info) = masternodes.get_mut(address) {
            let was_active = info.is_active;
            info.last_heartbeat = now;

            if !was_active {
                // Masternode came back online
                info.is_active = true;
                info.uptime_start = now;
                info!("✓ Masternode {} is back online", address);
            }

            // Persist to disk
            let key = format!("masternode:{}", address);
            let value =
                bincode::serialize(&info).map_err(|e| RegistryError::Storage(e.to_string()))?;
            self.db
                .insert(key.as_bytes(), value)
                .map_err(|e| RegistryError::Storage(e.to_string()))?;

            Ok(())
        } else {
            Err(RegistryError::NotFound)
        }
    }

    /// Get masternodes that were active for the ENTIRE block period
    pub async fn get_eligible_for_rewards(&self) -> Vec<(Masternode, String)> {
        let block_period_start = *self.block_period_start.read().await;
        let masternodes = self.masternodes.read().await;

        masternodes
            .values()
            .filter(|info| info.is_active && info.uptime_start <= block_period_start)
            .map(|info| (info.masternode.clone(), info.reward_address.clone()))
            .collect()
    }

    pub async fn start_new_block_period(&self) {
        let now = Self::now();
        *self.block_period_start.write().await = now;
        info!("✓ Started new block reward period at {}", now);
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub async fn get(&self, address: &str) -> Option<MasternodeInfo> {
        self.masternodes.read().await.get(address).cloned()
    }

    pub async fn list_all(&self) -> Vec<MasternodeInfo> {
        self.masternodes.read().await.values().cloned().collect()
    }

    #[allow(dead_code)]
    pub async fn list_active(&self) -> Vec<MasternodeInfo> {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| info.is_active)
            .cloned()
            .collect()
    }

    pub async fn set_peer_manager(&self, peer_manager: Arc<crate::peer_manager::PeerManager>) {
        *self.peer_manager.write().await = Some(peer_manager);
    }

    #[allow(dead_code)]
    pub async fn active_count(&self) -> usize {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| info.is_active)
            .count()
    }

    #[allow(dead_code)]
    pub async fn list_by_tier(&self, tier: MasternodeTier) -> Vec<MasternodeInfo> {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| {
                std::mem::discriminant(&info.masternode.tier) == std::mem::discriminant(&tier)
            })
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    pub async fn count(&self) -> usize {
        self.masternodes.read().await.len()
    }

    #[allow(dead_code)]
    pub async fn count_active(&self) -> usize {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| info.is_active)
            .count()
    }

    #[allow(dead_code)]
    pub async fn is_registered(&self, address: &str) -> bool {
        self.masternodes.read().await.contains_key(address)
    }
}

impl Clone for MasternodeRegistry {
    fn clone(&self) -> Self {
        Self {
            masternodes: self.masternodes.clone(),
            db: self.db.clone(),
            network: self.network,
            block_period_start: self.block_period_start.clone(),
            peer_manager: self.peer_manager.clone(),
        }
    }
}
