//! Masternode registry and management

#![allow(dead_code)]

use crate::types::{Masternode, MasternodeTier};
use crate::NetworkType;
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{info, warn};

const HEARTBEAT_INTERVAL_SECS: u64 = 60; // Masternodes must ping every 60 seconds
const MAX_MISSED_HEARTBEATS: u64 = 5; // Allow 5 missed heartbeats (5 minutes) before marking offline

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
                // Strip port from address to normalize (handles old entries with ports)
                let ip_only = info
                    .masternode
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&info.masternode.address)
                    .to_string();

                // If we already have this IP, keep the more recent one
                if let Some(existing) = nodes.get(&ip_only) {
                    if info.last_heartbeat > existing.last_heartbeat {
                        // This one is newer, replace it
                        let mut updated_info = info;
                        updated_info.masternode.address = ip_only.clone();
                        nodes.insert(ip_only, updated_info);
                    }
                    // Otherwise keep the existing one
                } else {
                    // New entry
                    let mut updated_info = info;
                    updated_info.masternode.address = ip_only.clone();
                    nodes.insert(ip_only, updated_info);
                }
            }
        }

        // Clean up old duplicate entries from disk
        let mut cleaned = 0;
        for item in db.scan_prefix(prefix).flatten() {
            if let Ok(key_str) = String::from_utf8(item.0.to_vec()) {
                // Extract address from key "masternode:ADDRESS"
                if let Some(addr) = key_str.strip_prefix("masternode:") {
                    if addr.contains(':') {
                        // This is an old entry with port, remove it
                        let _ = db.remove(item.0);
                        cleaned += 1;
                    }
                }
            }
        }

        if cleaned > 0 {
            tracing::info!(
                "🧹 Cleaned up {} duplicate masternode entries with ports",
                cleaned
            );
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
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30)); // Check every 30 seconds instead of 120
        loop {
            interval.tick().await;

            let now = Self::now();
            let mut masternodes = self.masternodes.write().await;
            let mut to_remove = Vec::new();

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
                } else {
                    // If offline for more than 24 hours, remove completely
                    // This is generous to allow network recovery after outages
                    // Masternodes that reconnect will be re-added automatically
                    let time_since_heartbeat = now - info.last_heartbeat;
                    if time_since_heartbeat > 86400 {
                        // 24 hours
                        to_remove.push(address.clone());
                    }
                }
            }

            // Remove stale masternodes
            for address in to_remove {
                masternodes.remove(&address);
                let key = format!("masternode:{}", address);
                let _ = self.db.remove(key.as_bytes());
                info!("🗑️  Removed stale masternode {} (offline >24hr)", address);
            }
        }
    }

    pub async fn register(
        &self,
        masternode: Masternode,
        reward_address: String,
    ) -> Result<(), RegistryError> {
        self.register_internal(masternode, reward_address, true)
            .await
    }

    /// Register a masternode with control over activation
    ///
    /// `should_activate`: if true, mark as active when registering/updating
    ///                    if false, only update info but don't change active status
    ///                    (used for peer exchange to avoid marking offline nodes as active)
    pub async fn register_internal(
        &self,
        masternode: Masternode,
        reward_address: String,
        should_activate: bool,
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

        // Get the count before we do any mutable operations
        let total_masternodes = nodes.len();

        // If already registered, update heartbeat (treat as heartbeat)
        if let Some(existing) = nodes.get_mut(&masternode.address) {
            let time_since_last = now - existing.last_heartbeat;

            // Only update last_heartbeat if should_activate is true (direct connection/heartbeat)
            // For peer exchange, we don't update heartbeat time
            if should_activate {
                existing.last_heartbeat = now;
            }

            if !existing.is_active && should_activate {
                existing.is_active = true;
                existing.uptime_start = now;
                info!(
                    "✅ Registered masternode {} (total: {}) - Tier: {:?}, Was offline for {}s, now ACTIVE at timestamp {}",
                    masternode.address,
                    total_masternodes,
                    masternode.tier,
                    time_since_last,
                    now
                );
            } else if should_activate {
                tracing::debug!(
                    "♻️  Heartbeat from {} - Tier: {:?}, Last seen: {}s ago, Active at: {}, Now: {}",
                    masternode.address,
                    masternode.tier,
                    time_since_last,
                    existing.uptime_start,
                    now
                );
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
            reward_address: reward_address.clone(),
            last_heartbeat: now,
            uptime_start: now,
            total_uptime: 0,
            is_active: should_activate, // Only mark as active if explicitly requested
        };

        // Persist to disk
        let key = format!("masternode:{}", masternode.address);
        let value = bincode::serialize(&info).map_err(|e| RegistryError::Storage(e.to_string()))?;

        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| RegistryError::Storage(e.to_string()))?;

        nodes.insert(masternode.address.clone(), info);
        let total_masternodes = nodes.len();

        info!(
            "✅ Registered masternode {} (total: {}) - NEW - Tier: {:?}, Reward address: {}, Active at timestamp: {}",
            masternode.address,
            total_masternodes,
            masternode.tier,
            reward_address,
            now
        );
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

    /// Get masternodes that are currently active (regardless of when they joined this period)
    pub async fn get_eligible_for_rewards(&self) -> Vec<(Masternode, String)> {
        let masternodes = self.masternodes.read().await;

        masternodes
            .values()
            .filter(|info| info.is_active)
            .map(|info| (info.masternode.clone(), info.reward_address.clone()))
            .collect()
    }

    pub async fn start_new_block_period(&self) {
        let now = Self::now();
        *self.block_period_start.write().await = now;
        info!("✓ Started new block reward period at {}", now);
    }

    /// Mark a masternode as inactive when connection is lost
    /// This ensures disconnected nodes don't receive rewards
    pub async fn mark_inactive_on_disconnect(&self, address: &str) -> Result<(), RegistryError> {
        let now = Self::now();
        let mut masternodes = self.masternodes.write().await;

        if let Some(info) = masternodes.get_mut(address) {
            if info.is_active {
                info.is_active = false;
                if info.uptime_start > 0 {
                    info.total_uptime += now - info.uptime_start;
                }
                warn!(
                    "⚠️  Masternode {} marked inactive (connection lost)",
                    address
                );

                // Persist to disk
                let key = format!("masternode:{}", address);
                let value =
                    bincode::serialize(&info).map_err(|e| RegistryError::Storage(e.to_string()))?;
                self.db
                    .insert(key.as_bytes(), value)
                    .map_err(|e| RegistryError::Storage(e.to_string()))?;
            }
            Ok(())
        } else {
            Err(RegistryError::NotFound)
        }
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

    pub async fn get_active_masternodes(&self) -> Vec<MasternodeInfo> {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| info.is_active)
            .cloned()
            .collect()
    }

    /// Get active masternodes that are currently connected
    /// This should be used for reward distribution to ensure only connected nodes get rewards
    pub async fn get_connected_active_masternodes(
        &self,
        connection_manager: &crate::network::connection_manager::ConnectionManager,
    ) -> Vec<MasternodeInfo> {
        let masternodes = self.masternodes.read().await;
        masternodes
            .values()
            .filter(|info| {
                // Must be active AND connected
                // Connection manager uses IP without port, so strip port from masternode address
                let ip_only = info
                    .masternode
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&info.masternode.address);
                info.is_active && connection_manager.is_connected(ip_only)
            })
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    pub async fn list_active(&self) -> Vec<MasternodeInfo> {
        self.get_active_masternodes().await
    }

    /// Get masternodes eligible for block rewards based on consensus participation
    /// This is consensus-safe: all nodes agree based on which masternodes voted in the
    /// previous block (on-chain data). This prevents:
    /// 1. Paying masternodes that have gone down (they won't vote)
    /// 2. Fork issues from different connection views (consensus votes are deterministic)
    ///
    /// NOTE: This makes the heartbeat mechanism obsolete since consensus participation
    /// proves a masternode is active and participating in the network.
    pub async fn get_masternodes_for_rewards(
        &self,
        blockchain: &crate::blockchain::Blockchain,
    ) -> Vec<MasternodeInfo> {
        // DETERMINISTIC MASTERNODE SELECTION FOR CONSENSUS
        // All nodes must agree on which masternodes receive rewards to avoid forks.
        // We select masternodes based on consensus participation in the previous block.

        let height = blockchain.get_height();
        let masternodes = self.masternodes.read().await;

        // Get all registered masternodes
        let mut all_nodes: Vec<MasternodeInfo> = masternodes.values().cloned().collect();

        if all_nodes.is_empty() {
            tracing::warn!(
                "⚠️  No masternodes registered for rewards at height {}",
                height
            );
            return vec![];
        }

        // For genesis and first few blocks, use registration time as fallback
        if height <= 3 {
            tracing::info!(
                "💰 Genesis/early blocks: using all {} registered masternodes",
                all_nodes.len()
            );
            all_nodes.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));
            return all_nodes;
        }

        // Use all registered masternodes
        // They must be registered (deterministic) and connected (each node validates this)
        all_nodes.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

        tracing::info!(
            "💰 Reward distribution at height {}: {} registered masternodes eligible",
            height,
            all_nodes.len()
        );

        all_nodes
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

    pub async fn get_local_address(&self) -> Option<String> {
        self.local_masternode_address.read().await.clone()
    }

    pub async fn set_local_masternode(&self, address: String) {
        *self.local_masternode_address.write().await = Some(address);
    }

    #[allow(dead_code)]
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
            // Send inventory message (just the height) instead of full block
            // Peers will request the full block if they need it
            let block_height = block.header.height;
            let msg = NetworkMessage::BlockInventory(block_height);
            match tx.send(msg) {
                Ok(0) => {
                    tracing::debug!(
                        "📡 Block {} inventory sent (no peers connected yet)",
                        block_height
                    );
                }
                Ok(receivers) => {
                    tracing::info!(
                        "📡 Broadcast block {} inventory to {} peer(s)",
                        block_height,
                        receivers
                    );
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

        // Use broadcast channel to send to all connected peers
        if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
            let msg = NetworkMessage::HeartbeatBroadcast(heartbeat.clone());
            match tx.send(msg) {
                Ok(receiver_count) => {
                    if receiver_count > 0 {
                        tracing::debug!("📡 Broadcast heartbeat to {} peer(s)", receiver_count);
                    }
                }
                Err(_) => {
                    tracing::trace!("No peers connected to receive heartbeat");
                }
            }
        }
    }

    #[allow(dead_code)]
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

    /// Broadcast any network message (used by consensus protocols)
    pub async fn broadcast_message(&self, msg: crate::network::message::NetworkMessage) {
        if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
            match tx.send(msg) {
                Ok(0) => {
                    tracing::debug!("📡 Message created (no peers connected)");
                }
                Ok(receivers) => {
                    tracing::debug!("📡 Broadcast message to {} peer(s)", receivers);
                }
                Err(_) => {
                    tracing::debug!("Message broadcast skipped (no active connections)");
                }
            }
        }
    }

    /// Receive and process a heartbeat broadcast from another masternode
    pub async fn receive_heartbeat_broadcast(
        &self,
        heartbeat: crate::heartbeat_attestation::SignedHeartbeat,
        health_ai: Option<&Arc<crate::ai::MasternodeHealthAI>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Update the masternode's last_heartbeat timestamp
        let mn_address = &heartbeat.masternode_address;

        let mut masternodes = self.masternodes.write().await;
        if let Some(info) = masternodes.get_mut(mn_address) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            info.last_heartbeat = now;
            info.is_active = true;
            tracing::debug!("💓 Updated last_heartbeat for masternode {}", mn_address);

            // Record heartbeat in AI (if available)
            if let Some(ai) = health_ai {
                if let Err(e) = ai.record_heartbeat(mn_address, now).await {
                    tracing::warn!("Failed to record AI heartbeat: {}", e);
                }
            }
        } else {
            // Masternode not in registry - register it from heartbeat
            // This ensures masternodes are discovered even if we missed their announcement
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Use the masternode address as the reward address (best guess from heartbeat)
            let reward_address = format!("TIME{}", &mn_address.replace('.', ""));

            let info = MasternodeInfo {
                masternode: crate::types::Masternode {
                    address: mn_address.clone(),
                    wallet_address: reward_address.clone(),
                    collateral: 0,                            // Unknown from heartbeat
                    tier: crate::types::MasternodeTier::Free, // Assume free tier
                    public_key: heartbeat.masternode_pubkey,
                    registered_at: now,
                },
                reward_address,
                last_heartbeat: now,
                uptime_start: now,
                total_uptime: 0,
                is_active: true,
            };

            let total = masternodes.len() + 1;
            masternodes.insert(mn_address.clone(), info.clone());

            // Persist to disk
            let key = format!("masternode:{}", mn_address);
            if let Ok(value) = bincode::serialize(&info) {
                let _ = self.db.insert(key.as_bytes(), value);
            }

            tracing::info!(
                "✅ Discovered masternode {} from heartbeat (total: {})",
                mn_address,
                total
            );
        }

        Ok(())
    }

    /// Receive and process an attestation broadcast
    pub async fn receive_attestation_broadcast(
        &self,
        attestation: crate::heartbeat_attestation::WitnessAttestation,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Update the witness masternode's last_heartbeat
        let attester = &attestation.witness_address;

        let mut masternodes = self.masternodes.write().await;
        if let Some(info) = masternodes.get_mut(attester) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            info.last_heartbeat = now;
            info.is_active = true;
            tracing::debug!(
                "✍️ Updated last_heartbeat for attesting masternode {}",
                attester
            );
        }

        Ok(())
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
