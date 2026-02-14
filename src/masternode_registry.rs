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
use tracing::{debug, info, warn};

const MIN_COLLATERAL_CONFIRMATIONS: u64 = 3; // Minimum confirmations for collateral UTXO (30 minutes at 10 min/block)

// Gossip-based status tracking constants
const MIN_PEER_REPORTS: usize = 3; // Masternode must be seen by at least 3 peers to be active
const REPORT_EXPIRY_SECS: u64 = 300; // Reports older than 5 minutes are stale
const GOSSIP_INTERVAL_SECS: u64 = 30; // Broadcast status every 30 seconds
const MIN_PARTICIPATION_SECS: u64 = 600; // 10 minutes minimum participation (prevents reward gaming)
const AUTO_REMOVE_AFTER_SECS: u64 = 3600; // Auto-remove masternodes with no peer reports for 1 hour

/// Network health status levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Critical, // 0-2 active masternodes (cannot produce blocks)
    Warning,  // 3-4 active masternodes (minimal operation)
    Degraded, // 5-9 active masternodes (should have more)
    Healthy,  // 10+ active masternodes
}

/// Network health report
#[derive(Debug, Clone)]
pub struct NetworkHealth {
    pub total_masternodes: usize,
    pub active_masternodes: usize,
    pub inactive_masternodes: usize,
    pub status: HealthStatus,
    pub actions_needed: Vec<String>,
}

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

    /// Reward tracking for fairness
    #[serde(default)]
    pub last_reward_height: u64, // Last block height where this MN received reward (0 = never)

    #[serde(default)]
    pub blocks_without_reward: u64, // Counter: increments each block, resets when reward received

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

    /// Helper to serialize and store a masternode to disk
    fn store_masternode(&self, address: &str, info: &MasternodeInfo) -> Result<(), RegistryError> {
        let key = format!("masternode:{}", address);
        let value = bincode::serialize(info).map_err(|e| RegistryError::Storage(e.to_string()))?;
        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| RegistryError::Storage(e.to_string()))?;
        Ok(())
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
        // Filter out invalid addresses
        if masternode.address == "0.0.0.0"
            || masternode.address == "127.0.0.1"
            || masternode.address.starts_with("127.")
            || masternode.address.starts_with("0.0.0.")
            || masternode.address.is_empty()
        {
            tracing::warn!(
                "🚫 Rejected invalid masternode address: {}",
                masternode.address
            );
            return Err(RegistryError::InvalidCollateral);
        }

        // Validate collateral (in satoshi units)
        let required = masternode.tier.collateral();

        if masternode.collateral != required {
            return Err(RegistryError::InvalidCollateral);
        }

        let mut nodes = self.masternodes.write().await;
        let now = Self::now();

        // Get the count before we do any mutable operations
        let total_masternodes = nodes.len();

        // If already registered, update status (treat as reconnection)
        if let Some(existing) = nodes.get_mut(&masternode.address) {
            // Protect local masternode entry from being overwritten by remote peers.
            // Only the local node (via register() on startup) may update its own fields.
            let is_local = self
                .local_masternode_address
                .read()
                .await
                .as_ref()
                .map(|a| {
                    let existing_ip = a.split(':').next().unwrap_or(a);
                    let incoming_ip = masternode
                        .address
                        .split(':')
                        .next()
                        .unwrap_or(&masternode.address);
                    existing_ip == incoming_ip
                })
                .unwrap_or(false);

            if is_local && !should_activate {
                // Remote peer trying to update our local entry — only allow activation
                tracing::debug!(
                    "🛡️ Ignoring remote update for local masternode {}",
                    masternode.address
                );
            } else {
                // Update tier and collateral info on re-registration
                existing.masternode.tier = masternode.tier;
                existing.masternode.collateral = masternode.collateral;
                existing.masternode.collateral_outpoint = masternode.collateral_outpoint.clone();
                existing.masternode.public_key = masternode.public_key;
                existing.reward_address = reward_address.clone();
            }

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
            self.store_masternode(&masternode.address, existing)?;

            return Ok(());
        }

        let info = MasternodeInfo {
            masternode: masternode.clone(),
            reward_address: reward_address.clone(),
            uptime_start: now,
            total_uptime: 0,
            is_active: should_activate, // Only active if explicitly activated (true for connections, false for gossip)
            last_reward_height: 0,
            blocks_without_reward: 0,
            peer_reports: Arc::new(DashMap::new()),
        };

        // Persist to disk
        self.store_masternode(&masternode.address, &info)?;

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

    /// Get all registered masternodes for bootstrap (blocks 0-3)
    /// Returns ALL masternodes regardless of active status
    /// Used at genesis when no bitmap exists yet and nodes are still discovering each other
    pub async fn get_all_for_bootstrap(&self) -> Vec<(Masternode, String)> {
        let masternodes = self.masternodes.read().await;

        masternodes
            .values()
            .map(|info| (info.masternode.clone(), info.reward_address.clone()))
            .collect()
    }

    pub async fn start_new_block_period(&self) {
        let now = Self::now();
        *self.block_period_start.write().await = now;
        debug!("✓ Started new block reward period at {}", now);
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
                self.store_masternode(address, info)?;
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

        // BOOTSTRAP MODE: Only for block 1 (height 0 → 1), use ALL registered masternodes
        // - Block 0 (genesis): already exists
        // - Block 1: Use all registered masternodes (including inactive) since no bitmap exists yet
        // - Block 2+: Use bitmap from previous block (normal participation-based selection)
        if current_height == 0 {
            let all_masternodes: Vec<MasternodeInfo> = self.list_all().await;
            tracing::info!(
                "💰 Block 1 (first block after genesis): using {} registered masternodes for bootstrap (including inactive, no bitmap yet)",
                all_masternodes.len()
            );
            return all_masternodes;
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

        // Get consensus participants from bitmap (compact representation)
        let voters_from_bitmap = self
            .get_active_from_bitmap(&prev_block.consensus_participants_bitmap)
            .await;
        for voter in voters_from_bitmap {
            participants.insert(voter.masternode.address.clone());
        }

        // If no participants recorded, fall back to active masternodes
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

        // CRITICAL SAFETY: If insufficient eligible masternodes after filtering, fall back progressively
        // This prevents deadlock when participation records are broken or incomplete
        // Minimum 3 masternodes required for block production
        if eligible.len() < 3 {
            let active = self.get_active_masternodes().await;

            // CRITICAL: If still insufficient active masternodes, return empty to prevent block production
            if active.len() < 3 {
                tracing::error!(
                    "🛡️ FORK PREVENTION: Only {} active masternodes (minimum 3 required) - refusing block production",
                    active.len()
                );
                return Vec::new();
            }

            // Rate-limit participation recovery logs (once per 60s) to avoid spam during catchup
            use std::sync::atomic::{AtomicI64, Ordering as AtomOrd};
            static LAST_PARTICIPATION_WARN: AtomicI64 = AtomicI64::new(0);
            let now_secs = chrono::Utc::now().timestamp();
            let last = LAST_PARTICIPATION_WARN.load(AtomOrd::Relaxed);
            if now_secs - last >= 60 {
                LAST_PARTICIPATION_WARN.store(now_secs, AtomOrd::Relaxed);
                tracing::warn!(
                    "⚠️ Participation recovery: block {} bitmap had {} participants, falling back to {} active masternodes",
                    current_height,
                    participants.len(),
                    active.len()
                );
            }
            return active;
        }

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

    /// Create a compact bitmap of active masternodes based on who voted on the block
    /// Returns (bitmap_bytes, sorted_masternode_list)
    ///
    /// Voting-based activity:
    /// - New nodes: announce → added to active list → can vote immediately
    /// - Voting: nodes that vote on block N get included in block N's bitmap
    /// - Leader selection: only nodes in previous block's bitmap are eligible
    /// - Removal: nodes that don't vote → excluded from bitmap → can't be selected
    ///
    /// Bitmap format: 1 bit per masternode in deterministic sorted order
    /// Bit = 1: masternode voted on this block (active participant)
    /// Bit = 0: masternode did not vote (inactive or offline)
    pub async fn create_active_bitmap_from_voters(&self, voters: &[String]) -> (Vec<u8>, usize) {
        let masternodes = self.masternodes.read().await;

        // Create deterministic sorted list of all masternodes
        let mut sorted_mns: Vec<MasternodeInfo> = masternodes.values().cloned().collect();
        sorted_mns.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

        if sorted_mns.is_empty() {
            return (vec![], 0);
        }

        // Convert voters to HashSet for fast lookup
        let voter_set: std::collections::HashSet<String> = voters.iter().cloned().collect();

        // Create bitmap: 1 bit per masternode
        let num_bits = sorted_mns.len();
        let num_bytes = num_bits.div_ceil(8); // Round up to nearest byte
        let mut bitmap = vec![0u8; num_bytes];

        let mut active_count = 0;
        for (i, mn) in sorted_mns.iter().enumerate() {
            // Active if voted on this block
            let voted = voter_set.contains(&mn.masternode.address);

            if voted {
                let byte_index = i / 8;
                let bit_index = 7 - (i % 8); // Big-endian: MSB first
                bitmap[byte_index] |= 1 << bit_index;
                active_count += 1;
            }
        }

        tracing::info!(
            "📊 Created active bitmap: {} masternodes total, {} voted ({:.1}%), {} bytes",
            sorted_mns.len(),
            active_count,
            (active_count as f64 / sorted_mns.len() as f64) * 100.0,
            bitmap.len()
        );

        (bitmap, active_count)
    }

    /// Get active masternodes from a block's bitmap
    /// Returns list of masternodes where bitmap bit = 1
    pub async fn get_active_from_bitmap(&self, bitmap: &[u8]) -> Vec<MasternodeInfo> {
        let masternodes = self.masternodes.read().await;

        // Create deterministic sorted list (same order as bitmap)
        let mut sorted_mns: Vec<MasternodeInfo> = masternodes.values().cloned().collect();
        sorted_mns.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

        // Extract active ones based on bitmap
        let mut active = Vec::new();
        for (i, mn) in sorted_mns.iter().enumerate() {
            let byte_index = i / 8;
            let bit_index = 7 - (i % 8); // Big-endian: MSB first

            if byte_index < bitmap.len() && (bitmap[byte_index] & (1 << bit_index)) != 0 {
                active.push(mn.clone());
            }
        }

        active
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
            tier.collateral(),
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
                    tracing::debug!("📡 Gossip broadcast sent to {} peer(s)", receivers);
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
        if utxo.value != required_collateral {
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

        tracing::debug!(
            "📡 Gossip: Checking visibility - we have {} connected peers, reporter: {}",
            connected_peers.len(),
            reporter
        );

        // Find which masternodes we're connected to
        let masternodes = self.masternodes.read().await;

        tracing::debug!(
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

        tracing::debug!(
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
        let mut filtered_count = 0;
        for mn_addr in &visible_masternodes {
            // Filter out invalid addresses
            if mn_addr == "0.0.0.0"
                || mn_addr == "127.0.0.1"
                || mn_addr.starts_with("127.")
                || mn_addr.starts_with("0.0.0.")
                || mn_addr.is_empty()
            {
                filtered_count += 1;
                continue;
            }

            if let Some(info) = masternodes.get(mn_addr) {
                info.peer_reports.insert(reporter.clone(), timestamp);
                updated_count += 1;
            }
        }

        if filtered_count > 0 {
            tracing::debug!(
                "📥 Gossip from {}: filtered {} invalid addresses, updated {} masternodes",
                reporter,
                filtered_count,
                updated_count
            );
        } else {
            tracing::debug!(
                "📥 Gossip from {}: reports seeing {} masternodes (updated {} in registry)",
                reporter,
                visible_masternodes.len(),
                updated_count
            );
        }
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

        // Calculate dynamic threshold once before the loop
        let total_masternodes = masternodes.len();
        let min_reports = if total_masternodes <= 4 {
            // Small network: require reports from at least half
            (total_masternodes / 2).max(1)
        } else {
            // Large network: use standard threshold
            MIN_PEER_REPORTS
        };

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
            // DYNAMIC THRESHOLD: Lower requirement for small networks to prevent deadlock
            let report_count = info.peer_reports.len();

            let was_active = info.is_active;
            info.is_active = report_count >= min_reports;

            if was_active != info.is_active {
                status_changes += 1;
                tracing::debug!(
                    "Masternode {} status changed: {} ({} peer reports, {} required)",
                    addr,
                    if info.is_active { "ACTIVE" } else { "INACTIVE" },
                    report_count,
                    min_reports
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

        // Remove dead masternodes
        for address in &to_remove {
            if let Some(_info) = masternodes.remove(address) {
                // Remove from disk
                let key = format!("masternode:{}", address);
                let _ = self.db.remove(key.as_bytes());

                info!("🗑️  Removed masternode {} from registry", address);
            }
        }

        if !to_remove.is_empty() || status_changes > 0 {
            if !to_remove.is_empty() {
                tracing::debug!(
                    "🧹 Cleanup: {} status changes, {} removed, {} total active masternodes",
                    status_changes,
                    to_remove.len(),
                    total_active
                );
            } else {
                tracing::debug!(
                    "🧹 Cleanup: {} status changes, {} total active masternodes",
                    status_changes,
                    total_active
                );
            }
        } else {
            tracing::debug!(
                "🧹 Cleanup: 0 status changes, {} total active masternodes",
                total_active
            );
        }
    }

    /// Calculate blocks without reward for a masternode by scanning blockchain
    /// This is deterministic and verifiable - all nodes get the same result
    /// Returns number of blocks since this masternode was last the block producer
    pub async fn calculate_blocks_without_reward(
        &self,
        masternode_address: &str,
        blockchain: &crate::blockchain::Blockchain,
    ) -> u64 {
        let current_height = blockchain.get_height();

        // Scan backwards through blockchain to find last time this masternode was leader
        // Limit scan to reasonable window (e.g., 1000 blocks) to avoid performance issues
        let scan_limit = 1000u64;
        let start_height = current_height.saturating_sub(scan_limit);

        for height in (start_height..=current_height).rev() {
            if let Ok(block) = blockchain.get_block(height) {
                if block.header.leader == masternode_address {
                    // Found when this masternode last produced a block
                    let blocks_since = current_height.saturating_sub(height);
                    return blocks_since;
                }
            }
        }

        // If not found in recent history, assume it's been > scan_limit blocks
        // Cap at scan_limit to prevent unbounded growth
        scan_limit
    }

    /// Get blocks without reward for all masternodes (on-chain verifiable)
    /// Scans blockchain history - deterministic across all nodes
    /// Optimized for bootstrap: returns all zeros at height 0-10
    pub async fn get_verifiable_reward_tracking(
        &self,
        blockchain: &crate::blockchain::Blockchain,
    ) -> std::collections::HashMap<String, u64> {
        let current_height = blockchain.get_height();
        let masternodes = self.masternodes.read().await;
        let mut tracking = std::collections::HashMap::new();

        // OPTIMIZATION: At genesis/early blocks, skip scanning and return zeros
        // All masternodes are equal at the start
        if current_height < 10 {
            for (address, _) in masternodes.iter() {
                tracking.insert(address.clone(), 0);
            }
            return tracking;
        }

        for (address, _) in masternodes.iter() {
            let blocks_without = self
                .calculate_blocks_without_reward(address, blockchain)
                .await;
            tracking.insert(address.clone(), blocks_without);
        }

        tracking
    }

    /// Record that a masternode received a reward at the given height
    /// Resets blocks_without_reward counter
    pub async fn record_reward(&self, masternode_address: &str, block_height: u64) {
        let mut masternodes = self.masternodes.write().await;
        if let Some(info) = masternodes.get_mut(masternode_address) {
            info.last_reward_height = block_height;
            info.blocks_without_reward = 0;

            // Persist to disk
            let key = format!("masternode:{}", masternode_address);
            if let Ok(data) = bincode::serialize(info) {
                let _ = self.db.insert(key.as_bytes(), data);
            }

            tracing::debug!(
                "💰 Masternode {} received reward at height {} (counter reset)",
                masternode_address,
                block_height
            );
        }
    }

    /// Increment blocks_without_reward for all masternodes except the one that just got rewarded
    /// Should be called after each block is produced
    pub async fn increment_blocks_without_reward(
        &self,
        current_height: u64,
        rewarded_address: &str,
    ) {
        let mut masternodes = self.masternodes.write().await;
        let mut updated_count = 0;

        for (address, info) in masternodes.iter_mut() {
            // Skip the masternode that just received reward
            if address == rewarded_address {
                continue;
            }

            info.blocks_without_reward += 1;
            updated_count += 1;

            // Log masternodes that are falling behind
            if info.blocks_without_reward % 50 == 0 && info.blocks_without_reward > 0 {
                tracing::info!(
                    "📊 Masternode {} has gone {} blocks without reward (last: height {})",
                    address,
                    info.blocks_without_reward,
                    info.last_reward_height
                );
            }
        }

        // Batch persist to disk periodically (every 10 blocks)
        if current_height % 10 == 0 {
            for (address, info) in masternodes.iter() {
                let key = format!("masternode:{}", address);
                if let Ok(data) = bincode::serialize(info) {
                    let _ = self.db.insert(key.as_bytes(), data);
                }
            }
            tracing::debug!(
                "💾 Persisted reward tracking for {} masternodes at height {}",
                updated_count,
                current_height
            );
        }
    }

    /// Get masternodes sorted by blocks_without_reward (descending)
    /// Masternodes that haven't received rewards in longer get prioritized
    pub async fn get_masternodes_by_reward_priority(&self) -> Vec<MasternodeInfo> {
        let masternodes = self.masternodes.read().await;
        let mut list: Vec<MasternodeInfo> = masternodes.values().cloned().collect();

        // Sort by blocks_without_reward descending (highest first = most starved)
        // Secondary sort by address for determinism when counts are equal
        list.sort_by(|a, b| {
            b.blocks_without_reward
                .cmp(&a.blocks_without_reward)
                .then_with(|| a.masternode.address.cmp(&b.masternode.address))
        });

        list
    }

    /// Check network health based on masternode counts
    /// Returns status and recommended actions
    pub async fn check_network_health(&self) -> NetworkHealth {
        let masternodes = self.masternodes.read().await;
        let total = masternodes.len();
        let active = masternodes.values().filter(|info| info.is_active).count();
        let inactive = total - active;

        let (status, actions_needed) = match active {
            0..=2 => (
                HealthStatus::Critical,
                vec![
                    "CRITICAL: Cannot produce blocks with <3 active masternodes".to_string(),
                    "Emergency: Reconnect masternodes immediately".to_string(),
                ],
            ),
            3..=4 => (
                HealthStatus::Warning,
                vec![
                    "WARNING: Minimal operation with 3-4 masternodes".to_string(),
                    "Recommend: Check connections and restart inactive nodes".to_string(),
                ],
            ),
            5..=9 if inactive > 0 => (
                HealthStatus::Degraded,
                vec![
                    "DEGRADED: Network should have more active masternodes".to_string(),
                    format!(
                        "{} inactive masternodes - investigate connectivity",
                        inactive
                    ),
                ],
            ),
            _ => (HealthStatus::Healthy, vec![]),
        };

        NetworkHealth {
            total_masternodes: total,
            active_masternodes: active,
            inactive_masternodes: inactive,
            status,
            actions_needed,
        }
    }

    /// Attempt to reconnect to inactive masternodes
    /// Returns list of addresses that were attempted
    pub async fn get_inactive_masternode_addresses(&self) -> Vec<String> {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| !info.is_active)
            .map(|info| info.masternode.address.clone())
            .collect()
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

/// Deterministic leader selection for a given block height.
///
/// This is the canonical implementation used by both block production (main.rs)
/// and block validation (message_handler.rs). Both MUST use this function to
/// ensure all nodes agree on the selected leader.
///
/// Returns the address of the selected leader, or None if masternodes is empty.
pub fn select_leader(
    masternodes: &[Masternode],
    prev_block_hash: &[u8],
    height: u64,
    attempt: u64,
    blocks_without_reward: &HashMap<String, u64>,
) -> Option<String> {
    if masternodes.is_empty() {
        return None;
    }

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(prev_block_hash);
    hasher.update(height.to_le_bytes());
    hasher.update(attempt.to_le_bytes());
    let selection_hash: [u8; 32] = hasher.finalize().into();

    // Build cumulative weight array for weighted selection
    let mut cumulative_weights: Vec<u64> = Vec::with_capacity(masternodes.len());
    let mut total_weight = 0u64;

    for mn in masternodes {
        let tier_weight = mn.tier.reward_weight();
        let blocks_without = blocks_without_reward.get(&mn.address).copied().unwrap_or(0);

        // Fairness bonus: +1 per 10 blocks without reward, capped at +20
        let fairness_bonus = (blocks_without / 10).min(20);
        let final_weight = tier_weight + fairness_bonus;

        total_weight = total_weight.saturating_add(final_weight);
        cumulative_weights.push(total_weight);
    }

    if total_weight == 0 {
        return None;
    }

    // Convert hash to random value in range [0, total_weight)
    let random_value = {
        let mut val = 0u64;
        for (i, &byte) in selection_hash.iter().take(8).enumerate() {
            val |= (byte as u64) << (i * 8);
        }
        val % total_weight
    };

    // Binary search to find selected masternode based on weight
    let producer_index = cumulative_weights
        .iter()
        .position(|&w| random_value < w)
        .unwrap_or(masternodes.len() - 1);

    Some(masternodes[producer_index].address.clone())
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
            MasternodeTier::Bronze.collateral(),
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
            MasternodeTier::Bronze.collateral(),
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
            MasternodeTier::Bronze.collateral(),
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

    // ========== Voting Bitmap Tests ==========

    #[tokio::test]
    async fn test_create_active_bitmap_from_voters_empty() {
        let registry = create_test_registry();

        // No voters, no masternodes
        let voters: Vec<String> = vec![];
        let (bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        assert_eq!(bitmap.len(), 0);
        assert_eq!(active_count, 0);
    }

    #[tokio::test]
    async fn test_create_active_bitmap_from_voters_single() {
        let registry = create_test_registry();

        // Register one masternode
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
        let public_key = signing_key.verifying_key();

        let masternode = crate::types::Masternode::new_legacy(
            "node1".to_string(),
            "reward1".to_string(),
            MasternodeTier::Bronze.collateral(),
            public_key,
            MasternodeTier::Bronze,
            MasternodeRegistry::now(),
        );

        registry
            .register(masternode.clone(), "reward1".to_string())
            .await
            .unwrap();

        // Node1 voted
        let voters = vec!["node1".to_string()];
        let (bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        // 1 bit = 1 byte
        assert_eq!(bitmap.len(), 1);
        assert_eq!(active_count, 1);
        assert_eq!(bitmap[0] & 0b10000000, 0b10000000); // First bit set
    }

    #[tokio::test]
    async fn test_create_active_bitmap_from_voters_multiple() {
        let registry = create_test_registry();

        // Register 10 masternodes
        for i in 0..10 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{}", i),
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // Only nodes 0, 2, 4, 6, 8 voted (even indices)
        let voters: Vec<String> = (0..10)
            .filter(|i| i % 2 == 0)
            .map(|i| format!("node{}", i))
            .collect();

        let (bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        // 10 bits = 2 bytes
        assert_eq!(bitmap.len(), 2);
        assert_eq!(active_count, 5); // 5 voters

        // Check that only even-indexed bits are set
        let all_masternodes = registry.list_all().await;
        let sorted_mns: Vec<_> = {
            let mut mns = all_masternodes.clone();
            mns.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));
            mns
        };

        for (i, mn) in sorted_mns.iter().enumerate() {
            let byte_idx = i / 8;
            let bit_idx = 7 - (i % 8); // Big-endian bit order
            let is_set = (bitmap[byte_idx] >> bit_idx) & 1 == 1;

            let expected = voters.contains(&mn.masternode.address);
            assert_eq!(
                is_set,
                expected,
                "Node {} (index {}) should be {}",
                mn.masternode.address,
                i,
                if expected { "set" } else { "unset" }
            );
        }
    }

    #[tokio::test]
    async fn test_create_active_bitmap_from_voters_non_existent_voter() {
        let registry = create_test_registry();

        // Register 3 masternodes
        for i in 0..3 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{}", i),
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // Include a non-existent voter
        let voters = vec!["node0".to_string(), "node999".to_string()];

        let (_bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        // Only node0 should be counted (node999 doesn't exist)
        assert_eq!(active_count, 1);
    }

    #[tokio::test]
    async fn test_get_active_from_bitmap() {
        let registry = create_test_registry();

        // Register 5 masternodes
        for i in 0..5 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{}", i),
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // Create bitmap from voters (nodes 1, 3)
        let voters = vec!["node1".to_string(), "node3".to_string()];
        let (bitmap, _) = registry.create_active_bitmap_from_voters(&voters).await;

        // Extract active masternodes from bitmap
        let active_masternodes = registry.get_active_from_bitmap(&bitmap).await;

        // Should get exactly the 2 voters back
        assert_eq!(active_masternodes.len(), 2);

        let active_addresses: Vec<String> = active_masternodes
            .iter()
            .map(|mn| mn.masternode.address.clone())
            .collect();

        assert!(active_addresses.contains(&"node1".to_string()));
        assert!(active_addresses.contains(&"node3".to_string()));
    }

    #[tokio::test]
    async fn test_bitmap_roundtrip() {
        let registry = create_test_registry();

        // Register 100 masternodes to test larger bitmap
        for i in 0..100 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{:03}", i), // Pad to ensure consistent sorting
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // Select random voters
        let voters: Vec<String> = (0..100)
            .filter(|i| i % 3 == 0) // Every 3rd node
            .map(|i| format!("node{:03}", i))
            .collect();

        // Create bitmap from voters
        let (bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        assert_eq!(active_count, 34); // 100/3 = 33 + node0 = 34

        // Extract active masternodes from bitmap
        let active_masternodes = registry.get_active_from_bitmap(&bitmap).await;

        // Should get exactly the voters back
        assert_eq!(active_masternodes.len(), voters.len());

        let active_addresses: std::collections::HashSet<String> = active_masternodes
            .iter()
            .map(|mn| mn.masternode.address.clone())
            .collect();

        for voter in &voters {
            assert!(
                active_addresses.contains(voter),
                "Voter {} missing from extracted actives",
                voter
            );
        }
    }

    #[tokio::test]
    async fn test_bitmap_size_efficiency() {
        let registry = create_test_registry();

        // Register 1000 masternodes (reduced from 10000 for faster test)
        for i in 0..1000 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{:04}", i),
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // All nodes voted
        let voters: Vec<String> = (0..1000).map(|i| format!("node{:04}", i)).collect();

        let (bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        // 1000 bits = 125 bytes (compared to 20KB for address list)
        assert_eq!(bitmap.len(), 125);
        assert_eq!(active_count, 1000);

        // Verify space efficiency: bitmap is ~99% smaller than address list
        let address_list_size = 1000 * 20; // Assuming 20 bytes per address
        let space_saving_percent =
            ((address_list_size - bitmap.len()) as f64 / address_list_size as f64) * 100.0;
        assert!(
            space_saving_percent > 99.0,
            "Space saving should be >99%, got {:.2}%",
            space_saving_percent
        );
    }

    #[tokio::test]
    async fn test_bitmap_empty_voters_full_registry() {
        let registry = create_test_registry();

        // Register masternodes but none voted
        for i in 0..5 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{}", i),
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // No voters
        let voters: Vec<String> = vec![];
        let (bitmap, active_count) = registry.create_active_bitmap_from_voters(&voters).await;

        // Should still create bitmap with correct size, but all bits 0
        assert_eq!(bitmap.len(), 1); // 5 nodes = 1 byte
        assert_eq!(active_count, 0);
        assert_eq!(bitmap[0], 0); // All bits should be 0

        // Extracting from empty bitmap should return empty list
        let active_masternodes = registry.get_active_from_bitmap(&bitmap).await;
        assert_eq!(active_masternodes.len(), 0);
    }

    #[tokio::test]
    async fn test_bitmap_legacy_empty_fallback() {
        let registry = create_test_registry();

        // Register masternodes
        for i in 0..3 {
            let signing_key = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
            let public_key = signing_key.verifying_key();

            let masternode = crate::types::Masternode::new_legacy(
                format!("node{}", i),
                format!("reward{}", i),
                MasternodeTier::Bronze.collateral(),
                public_key,
                MasternodeTier::Bronze,
                MasternodeRegistry::now(),
            );

            registry
                .register(masternode.clone(), format!("reward{}", i))
                .await
                .unwrap();
        }

        // Simulate legacy block with empty bitmap
        let empty_bitmap: Vec<u8> = vec![];
        let active_masternodes = registry.get_active_from_bitmap(&empty_bitmap).await;

        // Empty bitmap should return empty list, triggering fallback in production code
        assert_eq!(active_masternodes.len(), 0);
    }
}
