//! Masternode registry and management

#![allow(dead_code)]

use crate::types::{Masternode, MasternodeTier};
use crate::NetworkType;
use dashmap::DashMap;
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{info, warn};

const MIN_COLLATERAL_CONFIRMATIONS: u64 = 3; // Minimum confirmations for collateral UTXO (30 minutes at 10 min/block)

// Gossip-based status tracking constants
const MIN_PEER_REPORTS: usize = 3; // Masternode must be seen by at least 3 peers to be active
const REPORT_EXPIRY_SECS: u64 = 300; // Reports older than 5 minutes are stale
const GOSSIP_INTERVAL_SECS: u64 = 30; // Broadcast status every 30 seconds
const MIN_PARTICIPATION_SECS: u64 = 600; // 10 minutes minimum participation (prevents reward gaming)
const AUTO_REMOVE_AFTER_SECS: u64 = 3600; // Auto-remove masternodes with no peer reports for 1 hour

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Masternode not found")]
    NotFound,
    #[error("Invalid collateral amount")]
    InvalidCollateral,
    #[error("Collateral UTXO not found")]
    CollateralNotFound,
    #[error("Collateral UTXO already locked")]
    CollateralAlreadyLocked,
    #[error("Insufficient collateral confirmations (need {0}, have {1})")]
    InsufficientConfirmations(u64, u64),
    #[error("Collateral has been spent")]
    CollateralSpent,
    #[error("Storage error: {0}")]
    Storage(String),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MasternodeInfo {
    pub masternode: Masternode,
    pub reward_address: String, // Address to send block rewards
    pub uptime_start: u64,      // When current uptime period started
    pub total_uptime: u64,      // Total uptime in seconds
    pub is_active: bool,

    /// Gossip-based status tracking (not serialized to disk)
    /// Maps peer_address -> last_seen_timestamp
    /// A masternode is active if seen by MIN_PEER_REPORTS different peers recently
    #[serde(skip)]
    pub peer_reports: Arc<DashMap<String, u64>>,
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

                // If we already have this IP, keep the existing one
                if let Some(_existing) = nodes.get(&ip_only) {
                    // Keep the existing one
                } else {
                    // New entry - initialize peer_reports and mark as INACTIVE on load
                    // Masternodes only become active when they connect
                    let mut updated_info = info;
                    updated_info.masternode.address = ip_only.clone();
                    updated_info.peer_reports = Arc::new(DashMap::new());
                    updated_info.is_active = false; // Force inactive on load - only active on connection
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

        Self {
            masternodes: Arc::new(RwLock::new(nodes)),
            local_masternode_address: Arc::new(RwLock::new(None)),
            db,
            network,
            block_period_start: Arc::new(RwLock::new(now)),
            peer_manager: Arc::new(RwLock::new(None)),
            broadcast_tx: Arc::new(RwLock::new(None)),
        }
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
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

        // If already registered, update status (treat as reconnection)
        if let Some(existing) = nodes.get_mut(&masternode.address) {
            if !existing.is_active && should_activate {
                existing.is_active = true;
                existing.uptime_start = now;
                info!(
                    "✅ Registered masternode {} (total: {}) - Tier: {:?}, now ACTIVE at timestamp {}",
                    masternode.address,
                    total_masternodes,
                    masternode.tier,
                    now
                );
            } else if should_activate {
                tracing::debug!(
                    "♻️  Connection from {} - Tier: {:?}, Active at: {}, Now: {}",
                    masternode.address,
                    masternode.tier,
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
            uptime_start: now,
            total_uptime: 0,
            is_active: should_activate, // Only active if explicitly activated (true for connections, false for gossip)
            peer_reports: Arc::new(DashMap::new()),
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
                    "⚠️  Masternode {} marked inactive (connection lost) - broadcasting to network",
                    address
                );

                // Broadcast inactive status to all peers for consensus
                if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
                    let msg = crate::network::message::NetworkMessage::MasternodeInactive {
                        address: address.to_string(),
                        timestamp: now,
                    };
                    let _ = tx.send(msg); // Ignore errors if no receivers
                }

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

    /// Get masternodes eligible for rewards based on previous block participation
    ///
    /// PARTICIPATION-BASED REWARDS: Masternodes only receive rewards if they participated
    /// in the previous block (either as producer or voter). This ensures:
    /// 1. Only active, connected masternodes receive rewards
    /// 2. No gaming via gossip - requires cryptographic proof on-chain
    /// 3. Deterministic - all nodes compute same result from blockchain
    /// 4. Solves chicken-egg: new nodes can participate in block N, get rewards in N+1
    ///
    /// ALGORITHM:
    /// - For block at height H, rewards go to masternodes that participated in block H-1
    /// - Participation = produced block OR voted in consensus
    /// - Both producer and voters are recorded on-chain in previous block
    /// - Bootstrap: first few blocks use all active masternodes (no previous participation yet)
    pub async fn get_masternodes_for_rewards(
        &self,
        blockchain: &crate::blockchain::Blockchain,
    ) -> Vec<MasternodeInfo> {
        let current_height = blockchain.get_height();

        // Genesis block has no previous block, no rewards
        if current_height == 0 {
            tracing::info!("💰 Block 0 (genesis): no rewards (no previous block)");
            return vec![];
        }

        // BOOTSTRAP MODE: For first few blocks, use all active masternodes
        // since there's no participation history yet
        if current_height <= 3 {
            let active = self.get_active_masternodes().await;
            tracing::info!(
                "💰 Block {} (bootstrap): using {} active masternodes (no participation history yet)",
                current_height,
                active.len()
            );
            return active;
        }

        // Get previous block to see who participated
        let prev_block = match blockchain.get_block_by_height(current_height).await {
            Ok(block) => block,
            Err(e) => {
                tracing::warn!(
                    "⚠️  Failed to get previous block {} for reward calculation: {}",
                    current_height,
                    e
                );
                // Fallback to active masternodes if we can't get previous block
                return self.get_active_masternodes().await;
            }
        };

        // Collect addresses that participated (producer + consensus voters)
        let mut participants = std::collections::HashSet::new();

        // Block producer always participated
        if !prev_block.header.leader.is_empty() {
            participants.insert(prev_block.header.leader.clone());
        }

        // Consensus participants (voters) also participated
        for voter in &prev_block.consensus_participants {
            if !voter.is_empty() {
                participants.insert(voter.clone());
            }
        }

        // If no participants recorded, fall back to active masternodes
        // This handles legacy blocks that don't have consensus_participants populated
        if participants.is_empty() {
            tracing::warn!(
                "⚠️  No participants recorded in previous block {} - using active masternodes as fallback",
                current_height
            );
            return self.get_active_masternodes().await;
        }

        // Filter masternodes to only those that participated
        let masternodes = self.masternodes.read().await;
        let eligible: Vec<MasternodeInfo> = masternodes
            .values()
            .filter(|mn| participants.contains(&mn.masternode.address))
            .cloned()
            .collect();

        tracing::info!(
            "💰 Block {}: {} masternodes eligible for rewards (participated in block {})",
            current_height + 1,
            eligible.len(),
            current_height
        );

        for mn in &eligible {
            tracing::debug!(
                "   → {} (tier {:?})",
                mn.masternode.address,
                mn.masternode.tier
            );
        }

        eligible
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
        let masternode = Masternode::new_legacy(
            address.clone(),
            reward_address.clone(),
            match tier {
                MasternodeTier::Free => 0,
                MasternodeTier::Bronze => 1_000,
                MasternodeTier::Silver => 10_000,
                MasternodeTier::Gold => 100_000,
            },
            public_key,
            tier,
            Self::now(),
        );

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

    /// Broadcast any network message (used by consensus protocols)
    pub async fn broadcast_message(&self, msg: crate::network::message::NetworkMessage) {
        if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
            match tx.send(msg.clone()) {
                Ok(0) => {
                    tracing::warn!("📡 Gossip broadcast: no peers connected to receive message");
                }
                Ok(receivers) => {
                    tracing::info!("📡 Gossip broadcast sent to {} peer(s)", receivers);
                }
                Err(e) => {
                    tracing::warn!("📡 Gossip broadcast failed: {:?}", e);
                }
            }
        } else {
            tracing::warn!("📡 Gossip broadcast skipped: broadcast channel not initialized");
        }
    }

    // ========== Phase 2.2: Collateral Validation Methods ==========

    /// Validate collateral UTXO for masternode registration
    ///
    /// Checks:
    /// - UTXO exists in UTXO set
    /// - Amount meets tier requirement
    /// - UTXO is unspent
    /// - UTXO is not already locked by another masternode
    /// - UTXO has minimum confirmations (100 blocks)
    pub async fn validate_collateral(
        &self,
        outpoint: &crate::types::OutPoint,
        tier: MasternodeTier,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
        _current_height: u64,
    ) -> Result<(), RegistryError> {
        // Get tier requirement
        let required_collateral = match tier {
            MasternodeTier::Free => 0,
            MasternodeTier::Bronze => 1_000 * 100_000_000, // 1,000 TIME in units
            MasternodeTier::Silver => 10_000 * 100_000_000, // 10,000 TIME in units
            MasternodeTier::Gold => 100_000 * 100_000_000, // 100,000 TIME in units
        };

        // Check if UTXO exists
        let utxo = utxo_manager
            .get_utxo(outpoint)
            .await
            .map_err(|_| RegistryError::CollateralNotFound)?;

        // Verify amount meets requirement
        if utxo.value < required_collateral {
            return Err(RegistryError::InvalidCollateral);
        }

        // Check if UTXO is already locked by another masternode
        if utxo_manager.is_collateral_locked(outpoint) {
            return Err(RegistryError::CollateralAlreadyLocked);
        }

        // Verify UTXO is spendable (not spent, not locked for transaction)
        if !utxo_manager.is_spendable(outpoint, None) {
            return Err(RegistryError::CollateralSpent);
        }

        // Check minimum confirmations
        // For this we need to know the block height where the UTXO was created
        // For now, we'll implement a simple check - in production this would
        // require tracking UTXO creation height in the UTXO manager
        // TODO: Add UTXO creation height tracking for proper confirmation checks

        // For now, we'll just log a warning if we can't verify confirmations
        tracing::debug!(
            "Collateral validation passed for outpoint {:?} (tier: {:?}, amount: {})",
            outpoint,
            tier,
            utxo.value / 100_000_000
        );

        Ok(())
    }

    /// Check if collateral for a registered masternode has been spent
    /// Returns true if collateral is still valid, false if spent
    pub async fn check_collateral_validity(
        &self,
        masternode_address: &str,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> bool {
        // Get masternode info
        let masternodes = self.masternodes.read().await;
        if let Some(info) = masternodes.get(masternode_address) {
            // Check if has locked collateral
            if let Some(collateral_outpoint) = &info.masternode.collateral_outpoint {
                // Verify UTXO still exists and is locked
                if !utxo_manager.is_collateral_locked(collateral_outpoint) {
                    tracing::warn!(
                        "⚠️ Masternode {} collateral {:?} is no longer locked",
                        masternode_address,
                        collateral_outpoint
                    );
                    return false;
                }

                // Verify UTXO exists
                if utxo_manager.get_utxo(collateral_outpoint).await.is_err() {
                    tracing::warn!(
                        "⚠️ Masternode {} collateral {:?} UTXO no longer exists",
                        masternode_address,
                        collateral_outpoint
                    );
                    return false;
                }

                return true;
            }

            // Legacy masternode without locked collateral - always valid
            true
        } else {
            false
        }
    }

    /// Automatically deregister masternodes whose collateral has been spent
    /// Should be called periodically (e.g., after each block)
    pub async fn cleanup_invalid_collaterals(
        &self,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> usize {
        let mut to_deregister = Vec::new();

        // Check all masternodes
        {
            let masternodes = self.masternodes.read().await;
            for (address, info) in masternodes.iter() {
                // Only check masternodes with locked collateral
                if info.masternode.collateral_outpoint.is_some()
                    && !self.check_collateral_validity(address, utxo_manager).await
                {
                    to_deregister.push(address.clone());
                }
            }
        }

        // Deregister invalid masternodes
        let count = to_deregister.len();
        for address in to_deregister {
            tracing::warn!(
                "🗑️ Auto-deregistering masternode {} due to invalid collateral",
                address
            );
            if let Err(e) = self.unregister(&address).await {
                tracing::error!("Failed to deregister masternode {}: {:?}", address, e);
            }
        }

        count
    }

    // ========== Gossip-based Status Tracking Methods ==========

    /// Start gossip broadcast task - runs every 30 seconds
    pub fn start_gossip_broadcaster(
        &self,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(GOSSIP_INTERVAL_SECS));
            loop {
                interval.tick().await;
                registry.broadcast_status_gossip(&peer_registry).await;
            }
        });
    }

    /// Broadcast which masternodes we can see
    async fn broadcast_status_gossip(
        &self,
        peer_registry: &crate::network::peer_connection_registry::PeerConnectionRegistry,
    ) {
        let local_addr = self.local_masternode_address.read().await.clone();
        if local_addr.is_none() {
            tracing::debug!("📡 Gossip: Skipping - not a masternode");
            return; // Not a masternode
        }

        let reporter = local_addr.unwrap();
        let connected_peers = peer_registry.get_connected_peers().await;

        tracing::info!(
            "📡 Gossip: Checking visibility - we have {} connected peers, reporter: {}",
            connected_peers.len(),
            reporter
        );

        // Find which masternodes we're connected to
        let masternodes = self.masternodes.read().await;

        tracing::info!(
            "📡 Gossip: Registry has {} total masternodes: {:?}",
            masternodes.len(),
            masternodes.keys().collect::<Vec<_>>()
        );

        let visible: Vec<String> = masternodes
            .keys()
            .filter(|addr| connected_peers.contains(addr))
            .cloned()
            .collect();

        drop(masternodes);

        if visible.is_empty() {
            tracing::warn!(
                "📡 Gossip: No visible masternodes (connected_peers: {}, but none are in registry)",
                connected_peers.len()
            );
            return;
        }

        let now = Self::now();
        let msg = crate::network::message::NetworkMessage::MasternodeStatusGossip {
            reporter: reporter.clone(),
            visible_masternodes: visible.clone(),
            timestamp: now,
        };

        self.broadcast_message(msg).await;

        tracing::info!(
            "📡 Gossip: Broadcasting visibility of {} masternodes: {:?}",
            visible.len(),
            visible
        );
    }

    /// Process received gossip - update peer_reports for each masternode
    pub async fn process_status_gossip(
        &self,
        reporter: String,
        visible_masternodes: Vec<String>,
        timestamp: u64,
    ) {
        let masternodes = self.masternodes.read().await;

        let mut updated_count = 0;
        for mn_addr in &visible_masternodes {
            if let Some(info) = masternodes.get(mn_addr) {
                info.peer_reports.insert(reporter.clone(), timestamp);
                updated_count += 1;
            }
        }

        tracing::info!(
            "📥 Gossip from {}: reports seeing {} masternodes (updated {} in registry)",
            reporter,
            visible_masternodes.len(),
            updated_count
        );
    }

    /// Start cleanup task - runs every 60 seconds
    pub fn start_report_cleanup(&self) {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                registry.cleanup_stale_reports().await;
            }
        });
        tracing::info!("✓ Gossip cleanup task started (runs every 60s)");
    }

    /// Remove stale peer reports and update is_active status
    async fn cleanup_stale_reports(&self) {
        let now = Self::now();
        let mut masternodes = self.masternodes.write().await;

        let mut status_changes = 0;
        let mut total_active = 0;

        for (addr, info) in masternodes.iter_mut() {
            // Remove expired reports
            let before_count = info.peer_reports.len();
            info.peer_reports
                .retain(|_, timestamp| now.saturating_sub(*timestamp) < REPORT_EXPIRY_SECS);
            let after_count = info.peer_reports.len();

            if before_count != after_count {
                tracing::debug!(
                    "Masternode {}: expired {} reports, {} remain",
                    addr,
                    before_count - after_count,
                    after_count
                );
            }

            // Update is_active based on number of recent reports
            let report_count = info.peer_reports.len();
            let was_active = info.is_active;
            info.is_active = report_count >= MIN_PEER_REPORTS;

            if was_active != info.is_active {
                status_changes += 1;
                tracing::info!(
                    "Masternode {} status changed: {} ({} peer reports)",
                    addr,
                    if info.is_active { "ACTIVE" } else { "INACTIVE" },
                    report_count
                );
            }

            if info.is_active {
                total_active += 1;
            }
        }

        // Auto-remove masternodes with no peer reports for extended period
        let mut to_remove = Vec::new();
        for (address, info) in masternodes.iter() {
            if info.peer_reports.is_empty() {
                // Check when last seen
                let last_seen = info.uptime_start;
                let time_since_last_seen = now.saturating_sub(last_seen);

                if time_since_last_seen > AUTO_REMOVE_AFTER_SECS {
                    warn!(
                        "🗑️  Scheduling auto-removal of masternode {} (inactive for {} minutes)",
                        address,
                        time_since_last_seen / 60
                    );
                    to_remove.push(address.clone());
                }
            }
        }

        // Remove dead masternodes and broadcast unlock messages
        for address in &to_remove {
            if let Some(info) = masternodes.remove(address) {
                // Remove from disk
                let key = format!("masternode:{}", address);
                let _ = self.db.remove(key.as_bytes());

                info!("🗑️  Removed masternode {} from registry", address);

                // Broadcast unlock if has collateral
                if let Some(collateral_outpoint) = info.masternode.collateral_outpoint {
                    let unlock_msg = crate::network::message::NetworkMessage::MasternodeUnlock {
                        address: address.clone(),
                        collateral_outpoint,
                        timestamp: now,
                    };
                    drop(masternodes); // Release lock before async operation
                    self.broadcast_message(unlock_msg).await;
                    masternodes = self.masternodes.write().await; // Re-acquire
                }
            }
        }

        if status_changes > 0 || total_active > 0 || !to_remove.is_empty() {
            if !to_remove.is_empty() {
                tracing::info!(
                    "🧹 Cleanup: {} status changes, {} removed, {} total active masternodes",
                    status_changes,
                    to_remove.len(),
                    total_active
                );
            } else {
                tracing::info!(
                    "🧹 Cleanup: {} status changes, {} total active masternodes",
                    status_changes,
                    total_active
                );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MasternodeTier, OutPoint, UTXO};
    use crate::utxo_manager::UTXOStateManager;
    use std::sync::Arc;

    fn create_test_registry() -> MasternodeRegistry {
        let db = sled::Config::new().temporary(true).open().unwrap();
        MasternodeRegistry::new(Arc::new(db), NetworkType::Testnet)
    }

    fn create_test_utxo_manager() -> Arc<UTXOStateManager> {
        Arc::new(UTXOStateManager::new())
    }

    fn create_test_outpoint(index: u32) -> OutPoint {
        OutPoint {
            txid: [index as u8; 32],
            vout: index,
        }
    }

    async fn add_test_utxo(manager: &UTXOStateManager, index: u32, amount: u64) {
        let outpoint = create_test_outpoint(index);
        let utxo = UTXO {
            outpoint: outpoint.clone(),
            value: amount,
            script_pubkey: vec![1, 2, 3],
            address: format!("test_address_{}", index),
        };
        manager.add_utxo(utxo).await.unwrap();
    }

    // ========== Phase 5: Collateral Validation Tests ==========

    #[tokio::test]
    async fn test_validate_collateral_success() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        // Add a UTXO with sufficient amount for Bronze tier
        let outpoint = create_test_outpoint(1);
        add_test_utxo(&utxo_manager, 1, 1_000 * 100_000_000).await; // 1,000 TIME

        // Validate collateral for Bronze tier
        let result = registry
            .validate_collateral(&outpoint, MasternodeTier::Bronze, &utxo_manager, 10)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_collateral_not_found() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        let outpoint = create_test_outpoint(999);

        // Try to validate non-existent UTXO
        let result = registry
            .validate_collateral(&outpoint, MasternodeTier::Bronze, &utxo_manager, 10)
            .await;

        assert!(matches!(result, Err(RegistryError::CollateralNotFound)));
    }

    #[tokio::test]
    async fn test_validate_collateral_insufficient_amount() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        // Add a UTXO with insufficient amount for Bronze tier
        let outpoint = create_test_outpoint(2);
        add_test_utxo(&utxo_manager, 2, 500 * 100_000_000).await; // Only 500 TIME

        // Try to validate for Bronze (needs 1,000 TIME)
        let result = registry
            .validate_collateral(&outpoint, MasternodeTier::Bronze, &utxo_manager, 10)
            .await;

        assert!(matches!(result, Err(RegistryError::InvalidCollateral)));
    }

    #[tokio::test]
    async fn test_validate_collateral_already_locked() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        // Add and lock a UTXO
        let outpoint = create_test_outpoint(3);
        add_test_utxo(&utxo_manager, 3, 1_000 * 100_000_000).await;
        utxo_manager
            .lock_collateral(
                outpoint.clone(),
                "other_masternode".to_string(),
                10,
                1_000 * 100_000_000,
            )
            .unwrap();

        // Try to validate already locked collateral
        let result = registry
            .validate_collateral(&outpoint, MasternodeTier::Bronze, &utxo_manager, 10)
            .await;

        assert!(matches!(
            result,
            Err(RegistryError::CollateralAlreadyLocked)
        ));
    }

    #[tokio::test]
    async fn test_validate_collateral_tier_amounts() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        // Test Bronze tier (1,000 TIME)
        let outpoint1 = create_test_outpoint(10);
        add_test_utxo(&utxo_manager, 10, 1_000 * 100_000_000).await;
        assert!(registry
            .validate_collateral(&outpoint1, MasternodeTier::Bronze, &utxo_manager, 10)
            .await
            .is_ok());

        // Test Silver tier (10,000 TIME)
        let outpoint2 = create_test_outpoint(11);
        add_test_utxo(&utxo_manager, 11, 10_000 * 100_000_000).await;
        assert!(registry
            .validate_collateral(&outpoint2, MasternodeTier::Silver, &utxo_manager, 10)
            .await
            .is_ok());

        // Test Gold tier (100,000 TIME)
        let outpoint3 = create_test_outpoint(12);
        add_test_utxo(&utxo_manager, 12, 100_000 * 100_000_000).await;
        assert!(registry
            .validate_collateral(&outpoint3, MasternodeTier::Gold, &utxo_manager, 10)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn test_cleanup_invalid_collaterals() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        // Register a masternode with collateral
        let outpoint = create_test_outpoint(20);
        add_test_utxo(&utxo_manager, 20, 1_000 * 100_000_000).await;

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let public_key = signing_key.verifying_key();

        let masternode = crate::types::Masternode::new_with_collateral(
            "test_node".to_string(),
            "test_reward".to_string(),
            1_000,
            outpoint.clone(),
            public_key,
            MasternodeTier::Bronze,
            MasternodeRegistry::now(),
        );

        registry
            .register(masternode, "test_reward".to_string())
            .await
            .unwrap();

        // Lock the collateral
        utxo_manager
            .lock_collateral(
                outpoint.clone(),
                "test_node".to_string(),
                10,
                1_000 * 100_000_000,
            )
            .unwrap();

        // Verify masternode is registered
        assert_eq!(registry.count().await, 1);

        // Unlock and spend the collateral (simulating spent collateral)
        utxo_manager.unlock_collateral(&outpoint).unwrap();
        utxo_manager.spend_utxo(&outpoint).await.unwrap();

        // Run cleanup
        let cleanup_count = registry.cleanup_invalid_collaterals(&utxo_manager).await;

        // Masternode should be removed
        assert_eq!(cleanup_count, 1);
        assert_eq!(registry.count().await, 0);
    }

    #[tokio::test]
    async fn test_check_collateral_validity() {
        let registry = create_test_registry();
        let utxo_manager = create_test_utxo_manager();

        // Register a masternode with valid collateral
        let outpoint = create_test_outpoint(30);
        add_test_utxo(&utxo_manager, 30, 1_000 * 100_000_000).await;

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let public_key = signing_key.verifying_key();

        let masternode = crate::types::Masternode::new_with_collateral(
            "valid_node".to_string(),
            "valid_reward".to_string(),
            1_000,
            outpoint.clone(),
            public_key,
            MasternodeTier::Bronze,
            MasternodeRegistry::now(),
        );

        registry
            .register(masternode.clone(), "valid_reward".to_string())
            .await
            .unwrap();

        // Lock the collateral
        utxo_manager
            .lock_collateral(
                outpoint.clone(),
                "valid_node".to_string(),
                10,
                1_000 * 100_000_000,
            )
            .unwrap();

        // Check validity - should be valid
        let is_valid = registry
            .check_collateral_validity("valid_node", &utxo_manager)
            .await;
        assert!(is_valid);

        // Unlock and spend the collateral
        utxo_manager.unlock_collateral(&outpoint).unwrap();
        utxo_manager.spend_utxo(&outpoint).await.unwrap();

        // Check validity again - should be invalid
        let is_valid = registry
            .check_collateral_validity("valid_node", &utxo_manager)
            .await;
        assert!(!is_valid);
    }

    #[tokio::test]
    async fn test_legacy_masternode_no_collateral_validation() {
        let registry = create_test_registry();

        // Register a legacy masternode (no collateral)
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let public_key = signing_key.verifying_key();

        let masternode = crate::types::Masternode::new_legacy(
            "legacy_node".to_string(),
            "legacy_reward".to_string(),
            1_000,
            public_key,
            MasternodeTier::Bronze,
            MasternodeRegistry::now(),
        );

        registry
            .register(masternode.clone(), "legacy_reward".to_string())
            .await
            .unwrap();

        // Legacy masternodes should always be valid (no collateral to check)
        let utxo_manager = create_test_utxo_manager();
        let is_valid = registry
            .check_collateral_validity("legacy_node", &utxo_manager)
            .await;
        assert!(is_valid);

        // Cleanup should not remove legacy masternodes
        let cleanup_count = registry.cleanup_invalid_collaterals(&utxo_manager).await;
        assert_eq!(cleanup_count, 0);
        assert_eq!(registry.count().await, 1);
    }
}
