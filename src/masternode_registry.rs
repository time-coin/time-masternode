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
    local_masternode_address: Arc<RwLock<Option<String>>>, // Track which one is ours
    db: Arc<Db>,
    network: NetworkType,
    block_period_start: Arc<RwLock<u64>>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
    broadcast_tx: Arc<
        RwLock<Option<tokio::sync::broadcast::Sender<crate::network::message::NetworkMessage>>>,
    >,
}

impl MasternodeRegistry {
    pub fn new(db: Arc<Db>, network: NetworkType) -> Self {
        let now = Self::now();

        // Load existing masternodes from disk
        let prefix = b"masternode:";
        let mut nodes: HashMap<String, MasternodeInfo> = HashMap::new();

        for item in db.scan_prefix(prefix).flatten() {
            if let Ok(info) = bincode::deserialize::<MasternodeInfo>(&item.1) {
                nodes.insert(info.masternode.address.clone(), info);
            }
        }

        if !nodes.is_empty() {
            tracing::info!("📂 Loaded {} masternode(s) from disk", nodes.len());
        }

        let registry = Self {
            masternodes: Arc::new(RwLock::new(nodes)),
            local_masternode_address: Arc::new(RwLock::new(None)),
            db,
            network,
            block_period_start: Arc::new(RwLock::new(now)),
            peer_manager: Arc::new(RwLock::new(None)),
            broadcast_tx: Arc::new(RwLock::new(None)),
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
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(120));
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
        let now = Self::now();

        // If already registered, update heartbeat (treat as heartbeat)
        if let Some(existing) = nodes.get_mut(&masternode.address) {
            existing.last_heartbeat = now;
            if !existing.is_active {
                existing.is_active = true;
                existing.uptime_start = now;
                info!("✓ Masternode {} reactivated", masternode.address);
            }

            // Update on disk
            let key = format!("masternode:{}", masternode.address);
            let value =
                bincode::serialize(&existing).map_err(|e| RegistryError::Storage(e.to_string()))?;
            self.db
                .insert(key.as_bytes(), value)
                .map_err(|e| RegistryError::Storage(e.to_string()))?;

            return Ok(());
        }

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

    /// Count all registered masternodes (not just active ones)
    /// Used during genesis and catchup when heartbeat requirements are relaxed
    pub async fn total_count(&self) -> usize {
        self.masternodes.read().await.len()
    }

    #[allow(dead_code)]
    pub async fn get_all(&self) -> Vec<MasternodeInfo> {
        self.masternodes.read().await.values().cloned().collect()
    }

    pub async fn set_peer_manager(&self, peer_manager: Arc<crate::peer_manager::PeerManager>) {
        *self.peer_manager.write().await = Some(peer_manager);
    }

    pub async fn set_broadcast_channel(
        &self,
        tx: tokio::sync::broadcast::Sender<crate::network::message::NetworkMessage>,
    ) {
        *self.broadcast_tx.write().await = Some(tx);
    }

    pub async fn get_local_masternode(&self) -> Option<MasternodeInfo> {
        // Return the masternode marked as local
        if let Some(local_addr) = self.local_masternode_address.read().await.as_ref() {
            self.masternodes.read().await.get(local_addr).cloned()
        } else {
            None
        }
    }

    pub async fn set_local_masternode(&self, address: String) {
        *self.local_masternode_address.write().await = Some(address);
    }

    pub async fn register_masternode(
        &self,
        address: String,
        reward_address: String,
        tier: MasternodeTier,
        public_key: ed25519_dalek::VerifyingKey,
    ) -> Result<(), RegistryError> {
        let masternode = Masternode {
            address: address.clone(),
            wallet_address: reward_address.clone(),
            collateral: match tier {
                MasternodeTier::Free => 0,
                MasternodeTier::Bronze => 1_000,
                MasternodeTier::Silver => 10_000,
                MasternodeTier::Gold => 100_000,
            },
            tier,
            public_key,
            registered_at: Self::now(),
        };

        self.register(masternode, reward_address).await
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

    pub async fn broadcast_block(&self, block: crate::block::types::Block) {
        use crate::network::message::NetworkMessage;

        if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
            let msg = NetworkMessage::BlockAnnouncement(block);
            match tx.send(msg) {
                Ok(0) => {
                    tracing::debug!("📡 Block produced (no peers connected yet)");
                }
                Ok(receivers) => {
                    tracing::info!("📡 Broadcast block to {} connected peer(s)", receivers);
                }
                Err(_) => {
                    tracing::debug!("Broadcast channel closed (no active connections)");
                }
            }
        } else {
            tracing::debug!("⚠️  Cannot broadcast block - no broadcast channel set");
        }
    }

    pub async fn broadcast_heartbeat(
        &self,
        heartbeat: crate::heartbeat_attestation::SignedHeartbeat,
    ) {
        use crate::network::message::NetworkMessage;

        if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
            let msg = NetworkMessage::HeartbeatBroadcast(heartbeat);
            match tx.send(msg) {
                Ok(0) => {
                    tracing::debug!("💓 Heartbeat created (no peers to attest yet)");
                }
                Ok(receivers) => {
                    tracing::debug!("📡 Broadcast heartbeat to {} peer(s)", receivers);
                }
                Err(_) => {
                    tracing::debug!("Heartbeat broadcast skipped (no active connections)");
                }
            }
        }
    }

    pub async fn broadcast_attestation(
        &self,
        attestation: crate::heartbeat_attestation::WitnessAttestation,
    ) {
        use crate::network::message::NetworkMessage;

        if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
            let msg = NetworkMessage::HeartbeatAttestation(attestation);
            match tx.send(msg) {
                Ok(0) => {
                    tracing::debug!("✍️ Attestation created (no peers connected)");
                }
                Ok(receivers) => {
                    tracing::debug!("📡 Broadcast attestation to {} peer(s)", receivers);
                }
                Err(_) => {
                    tracing::debug!("Attestation broadcast skipped (no active connections)");
                }
            }
        }
    }
}

impl Clone for MasternodeRegistry {
    fn clone(&self) -> Self {
        Self {
            masternodes: self.masternodes.clone(),
            local_masternode_address: self.local_masternode_address.clone(),
            db: self.db.clone(),
            network: self.network,
            block_period_start: self.block_period_start.clone(),
            peer_manager: self.peer_manager.clone(),
            broadcast_tx: self.broadcast_tx.clone(),
        }
    }
}
