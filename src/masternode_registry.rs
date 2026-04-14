//! Masternode registry and management

#![allow(dead_code)]

use crate::types::{Masternode, MasternodeTier, OutPoint};
use crate::NetworkType;
use dashmap::DashMap;
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Queued collateral lock request: (outpoint, masternode_address, lock_height, amount)
type PendingCollateralLock = (OutPoint, String, u64, u64);

const MIN_COLLATERAL_CONFIRMATIONS: u64 = 3; // Minimum confirmations for collateral UTXO (30 minutes at 10 min/block)

// Gossip-based status tracking constants
const MIN_PEER_REPORTS: usize = 3; // Masternode must be seen by at least 3 peers to be active
const REPORT_EXPIRY_SECS: u64 = 300; // Reports older than 5 minutes are stale
const GOSSIP_INTERVAL_SECS: u64 = 30; // Broadcast status every 30 seconds
const MIN_PARTICIPATION_SECS: u64 = 600; // 10 minutes minimum participation (prevents reward gaming)
const AUTO_REMOVE_AFTER_SECS: u64 = 3600; // Auto-remove masternodes with no peer reports for 1 hour
const STARTUP_GRACE_PERIOD_SECS: u64 = 120; // Skip auto-removal during first 2 minutes after startup

// Reachability probe constants
/// TCP connect timeout for probing a peer's P2P port
const REACHABILITY_PROBE_TIMEOUT_SECS: u64 = 10;
/// How long a reachability result is cached before re-probing (10 minutes)
const REACHABILITY_RECHECK_SECS: u64 = 600;
/// Grace period after registration before reachability is enforced for rewards (5 minutes)
pub const REACHABILITY_GRACE_PERIOD_SECS: u64 = 300;

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
    #[error("Collateral outpoint already used by another masternode")]
    DuplicateCollateral,
    #[error("Insufficient collateral confirmations (need {0}, have {1})")]
    InsufficientConfirmations(u64, u64),
    #[error("Collateral has been spent")]
    CollateralSpent,
    #[error("Owner public key does not match collateral address")]
    OwnerMismatch,
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("IP cycling detected (AV3)")]
    IpCyclingRejected,
    #[error("Storage error: {0}")]
    Storage(String),
}

/// How a masternode was registered (handshake-based or on-chain transaction)
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum RegistrationSource {
    /// Registered via peer handshake (legacy, Free tier)
    #[default]
    Handshake,
    /// Registered via on-chain special transaction at the given block height
    OnChain(u64),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MasternodeInfo {
    pub masternode: Masternode,
    pub reward_address: String, // Address to send block rewards
    pub uptime_start: u64,      // When current uptime period started
    pub total_uptime: u64,      // Total uptime in seconds
    pub is_active: bool,

    /// Unix timestamp when this masternode's daemon last started.
    /// Reported via MasternodeAnnouncementV3 and used for real remote uptime display.
    #[serde(default)]
    pub daemon_started_at: u64,

    /// Reward tracking for fairness
    #[serde(default)]
    pub last_reward_height: u64, // Last block height where this MN received reward (0 = never)

    #[serde(default)]
    pub blocks_without_reward: u64, // Counter: increments each block, resets when reward received

    /// Block height at which this masternode was first registered.
    #[serde(default)]
    pub registration_height: u64,

    /// How this masternode was registered. On-chain takes precedence over handshake.
    #[serde(default)]
    pub registration_source: RegistrationSource,

    /// Gossip-based status tracking (not serialized to disk)
    /// Maps peer_address -> last_seen_timestamp
    /// A masternode is active if seen by MIN_PEER_REPORTS different peers recently
    #[serde(skip)]
    pub peer_reports: Arc<DashMap<String, u64>>,

    /// Hex-encoded Ed25519 public key of the masternode operator (node hot key).
    /// Set when a MasternodeReg tx includes a separate operator_pubkey (two-key model).
    /// None for legacy single-key registrations where owner == operator.
    /// During gossip/handshake, the announcing node's P2P key is verified against this.
    #[serde(default)]
    pub operator_pubkey: Option<String>,

    /// Whether this masternode's P2P port is publicly reachable from the outside.
    /// Set to true when an outbound connection succeeds (we dialed them), or after a
    /// successful reverse-probe for inbound-only connections.  Nodes that are only
    /// reachable inbound (behind NAT/firewall with no port forwarding) are excluded
    /// from block rewards because they are not contributing full network services.
    #[serde(default)]
    pub is_publicly_reachable: bool,

    /// Unix timestamp of the last reachability probe attempt (not serialized).
    /// Used to rate-limit re-probing to once per REACHABILITY_RECHECK_SECS.
    #[serde(skip)]
    pub reachability_checked_at: u64,

    /// Unix timestamp when this masternode was first seen (registered).
    /// Used for the reachability grace period: newly connected nodes get
    /// REACHABILITY_GRACE_PERIOD_SECS before the reachability check is enforced.
    #[serde(skip)]
    pub first_seen_at: u64,

    /// Unix timestamp of the last time this node was observed as active (connected).
    /// Updated whenever is_active transitions to true.
    /// Not persisted — starts at 0 after a daemon restart (no grace until node reconnects).
    /// Used for the reward-eligibility grace period: a node that briefly disconnects
    /// (e.g., due to a targeted attack) stays in the eligible pool for
    /// ELIGIBILITY_GRACE_SECS so it doesn't lose a block reward from a momentary drop.
    #[serde(skip)]
    pub last_seen_at: u64,
}

/// Buffered sled write sent to the background writer task.
/// All sled I/O from within async write-lock scope is sent here so tokio
/// workers are never blocked by disk operations.
enum SledWriteOp {
    Upsert { key: Vec<u8>, value: Vec<u8> },
    Remove { key: Vec<u8> },
}

pub struct MasternodeRegistry {
    masternodes: Arc<RwLock<HashMap<String, MasternodeInfo>>>,
    local_masternode_address: Arc<RwLock<Option<String>>>, // Track which one is ours
    /// Wallet (reward) address of the local masternode — persists across deregistration
    local_wallet_address: Arc<RwLock<Option<String>>>,
    /// Certificate for the local masternode (website-issued Ed25519 signature)
    local_certificate: Arc<RwLock<[u8; 64]>>,
    db: Arc<Db>,
    /// Background sled-writer channel. Sled writes are queued here and flushed
    /// asynchronously so the masternodes write lock is never held during disk I/O.
    sled_write_tx: tokio::sync::mpsc::UnboundedSender<SledWriteOp>,
    network: NetworkType,
    block_period_start: Arc<RwLock<u64>>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
    broadcast_tx: Arc<
        RwLock<Option<tokio::sync::broadcast::Sender<crate::network::message::NetworkMessage>>>,
    >,
    started_at: u64,
    /// Current blockchain height, updated externally. Used to set registration_height
    /// on newly registered masternodes for the anti-sybil maturity gate.
    current_height: Arc<std::sync::atomic::AtomicU64>,
    /// Collateral outpoints pending unlock (drained by periodic task with utxo_manager access)
    pending_collateral_unlocks: Arc<parking_lot::Mutex<Vec<OutPoint>>>,
    /// Collateral outpoints pending lock after a collateral change
    pending_collateral_locks: Arc<parking_lot::Mutex<Vec<PendingCollateralLock>>>,
    /// Consecutive blocks where each masternode's collateral UTXO was missing.
    /// Only deregister after this count reaches the threshold (avoids split-brain
    /// from transient UTXO-set divergence at block boundaries).
    collateral_miss_counts: Arc<DashMap<String, u32>>,
    /// Per-outpoint timestamp of the last IP migration.
    /// Prevents rapid flip-flop when two peers gossip the same collateral with different IPs.
    /// Key: "<txid>:<vout>", Value: Unix timestamp of the last accepted migration.
    collateral_migration_times: Arc<DashMap<String, u64>>,
    /// Per-outpoint: the source IP of the most recent accepted migration.
    /// Used to detect back-and-forth IP cycling attacks (AV3).
    /// Key: "<txid>:<vout>", Value: the IP the collateral migrated FROM last time.
    collateral_migration_from: Arc<DashMap<String, String>>,
    /// Per-outpoint timestamp of the most recent V4-proof eviction.
    /// Any free-tier IP migration targeting the same outpoint is blocked for
    /// POST_EVICTION_LOCKOUT_SECS (600 s) after a V4 eviction, closing the
    /// re-squatting oscillation loop (Attack Vector 14).
    /// Key: "<txid>:<vout>", Value: Unix timestamp of the eviction.
    post_eviction_lockout: Arc<DashMap<String, u64>>,
    /// Per-outpoint migration history within a sliding window (AV26 pool rotation defense).
    /// Key: "<txid>:<vout>", Value: (migration_count, window_start_unix_secs)
    /// Rejects outpoints that migrate more than MAX_MIGRATIONS_PER_WINDOW times in 30 minutes.
    collateral_migration_counts: Arc<DashMap<String, (u32, u64)>>,
    /// Per-/24-subnet count of currently registered Free-tier masternodes (AV25 flooding defense).
    /// Key: "A.B.C" (first three octets of the IP), Value: current registered count.
    /// Limits attacker subnets from flooding the registry with many Free-tier nodes.
    free_tier_subnet_counts: Arc<DashMap<String, u32>>,
    /// Per-IP Unix timestamp of the most recent Free-tier removal on disconnect (AV3 cycling defense).
    /// Key: IP string, Value: Unix timestamp of removal.
    /// Prevents rapid re-registration within FREE_TIER_RECONNECT_COOLDOWN_SECS of removal.
    /// Also doubles as a dedup guard: concurrent disconnect tasks for the same IP skip the second one.
    free_tier_reconnect_cooldown: Arc<DashMap<String, u64>>,
    /// Wakeup signal for the PHASE3 reconnection loop.  Fired when a paid-tier node
    /// disconnects so PHASE3 reconnects in milliseconds instead of waiting up to 30s.
    priority_reconnect_notify: Arc<tokio::sync::Notify>,
    /// Optional reference to the UTXO manager, used for collateral ownership verification.
    utxo_manager: Arc<RwLock<Option<Arc<crate::utxo_manager::UTXOStateManager>>>>,
    /// Sync-readable snapshot of active masternodes (is_active == true).
    /// Rebuilt inside every masternodes write lock scope so it is always consistent.
    /// Lets consensus sync functions read without block_in_place + block_on.
    cached_active: parking_lot::RwLock<Vec<MasternodeInfo>>,
    /// Sync-readable snapshot of all masternodes.
    cached_all: parking_lot::RwLock<Vec<MasternodeInfo>>,
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
                    // Set first_seen_at to now so nodes loaded from disk get a grace period
                    // before the reachability check is enforced (probe runs after 5 min warm-up).
                    updated_info.first_seen_at = now;
                    // Accumulate outstanding uptime from the previous session before marking inactive
                    if updated_info.is_active
                        && updated_info.uptime_start > 0
                        && updated_info.uptime_start <= now
                    {
                        updated_info.total_uptime += now - updated_info.uptime_start;
                    }
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

        // Migration: write canonical anchors for any existing paid-tier node that
        // doesn't have one yet.  Prior versions only wrote anchors via on-chain
        // MasternodeReg txs; without an in-sled anchor a Free-tier gossip claim can
        // bypass the canonical-anchor check and migrate the entry.
        let mut anchors_written = 0usize;
        for info in nodes.values() {
            if info.masternode.tier == crate::types::MasternodeTier::Free {
                continue;
            }
            if let Some(ref outpoint) = info.masternode.collateral_outpoint {
                let outpoint_key = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
                let anchor_key = format!("collateral_anchor:{}", outpoint_key);
                if db.get(anchor_key.as_bytes()).ok().flatten().is_none() {
                    let _ = db.insert(anchor_key.as_bytes(), info.masternode.address.as_bytes());
                    anchors_written += 1;
                }
            }
        }
        if anchors_written > 0 {
            tracing::info!(
                "🔒 Wrote {} missing canonical anchor(s) for existing paid-tier node(s)",
                anchors_written
            );
        }

        // Populate AV25 Free-tier subnet counts from the nodes loaded from disk
        // so the cap is immediately enforced even before new registrations arrive.
        let free_tier_subnet_counts: DashMap<String, u32> = DashMap::new();
        for info in nodes.values() {
            if info.masternode.tier == crate::types::MasternodeTier::Free {
                let subnet = Self::free_tier_subnet(&info.masternode.address);
                *free_tier_subnet_counts.entry(subnet).or_insert(0) += 1;
            }
        }

        // Build initial sync caches from the loaded nodes.
        let init_active: Vec<MasternodeInfo> =
            nodes.values().filter(|i| i.is_active).cloned().collect();
        let init_all: Vec<MasternodeInfo> = nodes.values().cloned().collect();

        // Spawn background sled-writer task. All sled inserts and removes are
        // queued via sled_write_tx so the masternodes write lock is never held
        // during disk I/O, preventing tokio worker starvation under gossip floods.
        let (sled_write_tx, mut sled_write_rx) =
            tokio::sync::mpsc::unbounded_channel::<SledWriteOp>();
        let db_bg = db.clone();
        tokio::runtime::Handle::current().spawn(async move {
            while let Some(op) = sled_write_rx.recv().await {
                match op {
                    SledWriteOp::Upsert { key, value } => {
                        if let Err(e) = db_bg.insert(key, value) {
                            tracing::warn!("⚠️ Sled background write failed: {}", e);
                        }
                    }
                    SledWriteOp::Remove { key } => {
                        if let Err(e) = db_bg.remove(key) {
                            tracing::warn!("⚠️ Sled background remove failed: {}", e);
                        }
                    }
                }
            }
        });

        Self {
            masternodes: Arc::new(RwLock::new(nodes)),
            local_masternode_address: Arc::new(RwLock::new(None)),
            local_wallet_address: Arc::new(RwLock::new(None)),
            local_certificate: Arc::new(RwLock::new([0u8; 64])),
            db,
            sled_write_tx,
            network,
            block_period_start: Arc::new(RwLock::new(now)),
            peer_manager: Arc::new(RwLock::new(None)),
            broadcast_tx: Arc::new(RwLock::new(None)),
            started_at: now,
            current_height: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            pending_collateral_unlocks: Arc::new(parking_lot::Mutex::new(Vec::new())),
            pending_collateral_locks: Arc::new(parking_lot::Mutex::new(Vec::new())),
            collateral_miss_counts: Arc::new(DashMap::new()),
            collateral_migration_times: Arc::new(DashMap::new()),
            collateral_migration_from: Arc::new(DashMap::new()),
            post_eviction_lockout: Arc::new(DashMap::new()),
            collateral_migration_counts: Arc::new(DashMap::new()),
            free_tier_subnet_counts: Arc::new(free_tier_subnet_counts),
            free_tier_reconnect_cooldown: Arc::new(DashMap::new()),
            priority_reconnect_notify: Arc::new(tokio::sync::Notify::new()),
            utxo_manager: Arc::new(RwLock::new(None)),
            cached_active: parking_lot::RwLock::new(init_active),
            cached_all: parking_lot::RwLock::new(init_all),
        }
    }

    /// Inject the UTXO manager after construction so that collateral ownership
    /// can be verified during gossip registration without circular dependencies.
    pub async fn set_utxo_manager(&self, utxo_manager: Arc<crate::utxo_manager::UTXOStateManager>) {
        *self.utxo_manager.write().await = Some(utxo_manager);
    }

    /// Return the priority-reconnect notify handle so the NetworkClient can
    /// call `notify_one()` from its PHASE3 select loop.
    pub fn priority_reconnect_notify(&self) -> Arc<tokio::sync::Notify> {
        self.priority_reconnect_notify.clone()
    }

    /// Remove inactive Free-tier nodes that have been offline longer than
    /// `max_inactive_secs`.  Called periodically by the health-monitoring task
    /// so stale Free-tier entries don't accumulate in the registry indefinitely.
    pub async fn clean_stale_free_tier_nodes(&self, max_inactive_secs: u64) {
        let now = Self::now();
        let mut to_remove: Vec<String> = Vec::new();
        {
            let nodes = self.masternodes.read().await;
            for (addr, info) in nodes.iter() {
                if info.masternode.tier == crate::types::MasternodeTier::Free
                    && !info.is_active
                    && info.last_seen_at > 0
                    && now.saturating_sub(info.last_seen_at) > max_inactive_secs
                {
                    to_remove.push(addr.clone());
                }
            }
        }
        if to_remove.is_empty() {
            return;
        }
        let mut nodes = self.masternodes.write().await;
        for addr in &to_remove {
            nodes.remove(addr);
            tracing::debug!(
                "🧹 Removed stale inactive Free-tier node {} (offline >{}s)",
                addr, max_inactive_secs
            );
        }
        self.rebuild_node_caches(&nodes);
        drop(nodes);
        // Remove persisted entries
        for addr in &to_remove {
            self.sled_remove_bg(format!("masternode:{}", addr).into_bytes());
        }
    }

    /// Record that a V4-proof eviction just occurred for `outpoint_key`
    /// (formatted as `"<txid_hex>:<vout>"`).
    ///
    /// Any free-tier IP migration targeting the same outpoint is blocked for
    /// 10 minutes after this call, preventing the evicted squatter (or a
    /// confederate) from immediately re-squatting under a different IP.
    pub fn record_v4_eviction(&self, outpoint_key: &str) {
        let now = Self::now();
        self.post_eviction_lockout
            .insert(outpoint_key.to_string(), now);
        tracing::debug!("Post-eviction lockout armed for {} (10 min)", outpoint_key);
    }

    fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Rebuild both sync caches from a snapshot of the masternodes map.
    ///
    /// Call this **while holding the `masternodes` write guard** so the cache is
    /// atomically consistent with the mutation that just completed.
    fn rebuild_node_caches(&self, nodes: &HashMap<String, MasternodeInfo>) {
        let active: Vec<MasternodeInfo> =
            nodes.values().filter(|i| i.is_active).cloned().collect();
        let all: Vec<MasternodeInfo> = nodes.values().cloned().collect();
        *self.cached_active.write() = active;
        *self.cached_all.write() = all;
    }

    /// Synchronous read of active masternodes (no await required).
    ///
    /// Backed by `rebuild_node_caches()` which is called after every write to
    /// `masternodes`, so this is always consistent with the async `list_active()`.
    pub fn active_masternodes_cached(&self) -> Vec<MasternodeInfo> {
        self.cached_active.read().clone()
    }

    /// Synchronous read of all masternodes (no await required).
    pub fn all_masternodes_cached(&self) -> Vec<MasternodeInfo> {
        self.cached_all.read().clone()
    }

    /// Extract the /24 subnet prefix from an IP string.
    /// Handles "IP:port" format (strips port first).
    /// Returns "A.B.C" for valid IPv4 addresses; falls back to the raw IP.
    fn free_tier_subnet(ip: &str) -> String {
        let ip_only = ip.split(':').next().unwrap_or(ip);
        let parts: Vec<&str> = ip_only.split('.').collect();
        if parts.len() >= 3 {
            format!("{}.{}.{}", parts[0], parts[1], parts[2])
        } else {
            ip_only.to_string()
        }
    }

    /// AV25: Fast (lock-free) check whether a /24 subnet is already at the Free-tier cap.
    /// Used by the message handler to drop excess announcements before acquiring registry locks.
    pub fn is_free_tier_subnet_at_cap(&self, ip: &str) -> bool {
        const MAX_FREE_TIER_PER_SUBNET: u32 = 5;
        let subnet = Self::free_tier_subnet(ip);
        self.free_tier_subnet_counts
            .get(&subnet)
            .map(|c| *c >= MAX_FREE_TIER_PER_SUBNET)
            .unwrap_or(false)
    }

    /// Update the current blockchain height. Called from block production loop
    /// so that newly registered masternodes get an accurate registration_height.
    pub fn update_height(&self, height: u64) {
        self.current_height
            .store(height, std::sync::atomic::Ordering::Relaxed);
    }

    /// Drain pending collateral unlock and lock requests. Call this periodically with
    /// access to the UTXOStateManager to actually unlock/lock the collateral UTXOs.
    pub fn drain_pending_unlocks(
        &self,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> usize {
        let outpoints: Vec<OutPoint> = self.pending_collateral_unlocks.lock().drain(..).collect();
        let locks: Vec<PendingCollateralLock> =
            self.pending_collateral_locks.lock().drain(..).collect();
        let count = outpoints.len() + locks.len();

        for outpoint in outpoints {
            if let Err(e) = utxo_manager.unlock_collateral(&outpoint) {
                tracing::debug!(
                    "Could not unlock collateral {}:{}: {:?} (may already be unlocked)",
                    hex::encode(outpoint.txid),
                    outpoint.vout,
                    e
                );
            }
        }

        // Lock new collateral outpoints queued during collateral changes
        for (outpoint, mn_address, lock_height, amount) in locks {
            match utxo_manager.lock_collateral(
                outpoint.clone(),
                mn_address.clone(),
                lock_height,
                amount,
            ) {
                Ok(()) => {
                    tracing::info!(
                        "🔒 Locked new collateral {}:{} for {}",
                        hex::encode(outpoint.txid),
                        outpoint.vout,
                        mn_address
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "Could not lock new collateral {}:{} for {}: {:?} (will retry via auto-lock)",
                        hex::encode(outpoint.txid),
                        outpoint.vout,
                        mn_address,
                        e
                    );
                }
            }
        }

        count
    }

    /// Get the network type this registry is configured for.
    pub fn network(&self) -> crate::NetworkType {
        self.network
    }

    /// Queue a sled key/value insertion via the background writer.
    /// Never blocks; errors are logged by the background task.
    fn sled_insert_bg(&self, key: Vec<u8>, value: Vec<u8>) {
        let _ = self
            .sled_write_tx
            .send(SledWriteOp::Upsert { key, value });
    }

    /// Queue a sled key removal via the background writer.
    fn sled_remove_bg(&self, key: Vec<u8>) {
        let _ = self.sled_write_tx.send(SledWriteOp::Remove { key });
    }

    /// Helper to serialize and enqueue a masternode write via the background sled writer.
    /// Serialization happens synchronously (cheap); disk I/O is deferred.
    fn store_masternode(&self, address: &str, info: &MasternodeInfo) -> Result<(), RegistryError> {
        let key = format!("masternode:{}", address).into_bytes();
        let value = bincode::serialize(info).map_err(|e| RegistryError::Storage(e.to_string()))?;
        self.sled_insert_bg(key, value);
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

        // AV3: Per-IP reconnect cooldown for Free-tier nodes.
        // Attackers cycle 60+ Free-tier IPs through rapid disconnect/reconnect to game
        // the registry and inflate quorum denominators.  After a Free-tier node is removed
        // on disconnect, block it from re-registering for 30 seconds.
        if masternode.tier == crate::types::MasternodeTier::Free {
            const FREE_TIER_RECONNECT_COOLDOWN_SECS: u64 = 30;
            let pre_now = Self::now();
            if let Some(removed_at) = self
                .free_tier_reconnect_cooldown
                .get(&masternode.address)
            {
                let elapsed = pre_now.saturating_sub(*removed_at);
                if elapsed < FREE_TIER_RECONNECT_COOLDOWN_SECS {
                    tracing::debug!(
                        "⏳ [AV3] Free-tier {} reconnect rejected ({}s cooldown, {}s elapsed)",
                        masternode.address,
                        FREE_TIER_RECONNECT_COOLDOWN_SECS,
                        elapsed
                    );
                    return Err(RegistryError::IpCyclingRejected);
                }
            }
        }

        // Pre-fetch the UTXO address AND local masternode address BEFORE taking the
        // write lock.  SledUtxoStorage::get_utxo uses spawn_blocking internally; any
        // await inside masternodes.write().await starves queued tasks.
        let prefetched_utxo_addr: Option<String> =
            if let Some(ref outpoint) = masternode.collateral_outpoint {
                let utxo_mgr_guard = self.utxo_manager.read().await;
                if let Some(ref utxo_manager) = *utxo_mgr_guard {
                    utxo_manager.get_utxo(outpoint).await.ok().map(|u| u.address)
                } else {
                    None
                }
            } else {
                None
            };

        // Also pre-fetch local_masternode_address to avoid a nested async lock
        // inside masternodes.write().await.
        let local_ip: Option<String> = self
            .local_masternode_address
            .read()
            .await
            .as_ref()
            .map(|a| a.split(':').next().unwrap_or(a).to_string());

        let mut nodes = self.masternodes.write().await;
        let now = Self::now();

        if let Some(ref outpoint) = masternode.collateral_outpoint {
            let outpoint_key = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);

            // ── Canonical-anchor check ────────────────────────────────────────────
            // The first address to register a collateral outpoint (either via on-chain
            // MasternodeReg tx or via gossip) becomes the permanent "anchor" stored in
            // sled under `collateral_anchor:{outpoint_key}`.  Any subsequent gossip
            // claim from a *different* address is rejected outright — regardless of
            // registered_at timestamp — so that a bad actor cannot gain higher-tier
            // rewards by copying someone else's collateral outpoint.
            //
            // Legitimate IP migrations (same operator, new IP) must use an on-chain
            // MasternodeReg special transaction that is verified with the owner's
            // private key (see apply_masternode_reg / validate_masternode_reg).
            let anchor_db_key = format!("collateral_anchor:{}", outpoint_key);
            let anchor_addr: Option<String> = self
                .db
                .get(anchor_db_key.as_bytes())
                .ok()
                .flatten()
                .and_then(|v| String::from_utf8(v.to_vec()).ok());

            if let Some(ref canonical) = anchor_addr {
                let canonical_ip = canonical.split(':').next().unwrap_or(canonical);
                let incoming_ip = masternode
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&masternode.address);
                if canonical_ip != incoming_ip {
                    // Before rejecting, check if the incoming node can prove ownership of
                    // the collateral UTXO via its output address.  The UTXO was created by
                    // the GUI wallet sending TIME to itself — the output address is the wallet
                    // address of the real owner.  A squatter gossips a txid they don't own;
                    // they can't match the UTXO's output address without controlling that key.
                    //
                    // If the incoming wallet_address matches the UTXO's output address, the
                    // incoming node is the legitimate owner — evict the squatter and re-anchor.
                    // Use the pre-fetched UTXO address (looked up before taking the write lock
                    // to avoid spawn_blocking inside masternodes.write().await).
                    let utxo_addr = prefetched_utxo_addr.clone();

                    if let Some(ref utxo_address) = utxo_addr {
                        if !utxo_address.is_empty() && utxo_address == &masternode.wallet_address {
                            // The UTXO output address equals the wallet/reward address, which
                            // is publicly visible in block rewards. Any node can forge a
                            // matching wallet_address regardless of what tier they claim.
                            // Only V4 cryptographic proof (verified upstream in
                            // message_handler) or an on-chain MasternodeReg tx can
                            // legitimately displace a paid-tier canonical holder.
                            //
                            // Exception: our own daemon is allowed to evict a squatter on
                            // startup.  In that path the squatter is first removed via
                            // unregister() (which clears the sled anchor), so typically no
                            // anchor exists when we re-register ourselves — this guard only
                            // fires in edge-case races where the anchor survived.

                            // Guard: the local node can NEVER be evicted from its own collateral
                            // via gossip UTXO check.  masternode.conf is ground truth; a remote
                            // node can forge any wallet_address to match the UTXO output address,
                            // and since our hot-wallet address needn't equal the cold-storage
                            // address used to fund the collateral, the UTXO match can be wrong.
                            // Only the local node itself (or an on-chain MasternodeReg tx) can
                            // displace us — handled by the same-address reconnect path above.
                            let canonical_is_local = local_ip
                                .as_deref()
                                .map(|l| l == canonical_ip)
                                .unwrap_or(false);
                            if canonical_is_local {
                                let spam_key =
                                    format!("hijack_warn:{}:{}", outpoint_key, incoming_ip);
                                let last_warned = self
                                    .collateral_migration_times
                                    .get(&spam_key)
                                    .map(|v| *v)
                                    .unwrap_or(0);
                                if now.saturating_sub(last_warned) >= 300 {
                                    self.collateral_migration_times.insert(spam_key, now);
                                    tracing::warn!(
                                        "🛡️ Gossip cannot evict local masternode {} from its own \
                                         collateral {} via UTXO check — rejected gossip from {} \
                                         (use on-chain MasternodeReg to migrate)",
                                        canonical_ip,
                                        outpoint_key,
                                        masternode.address,
                                    );
                                }
                                return Err(RegistryError::CollateralAlreadyLocked);
                            }

                            // Dual-claimant stalemate guard (AV20): if the canonical holder's
                            // wallet address ALSO matches the UTXO address, both nodes control
                            // the same key (duplicate masternode.conf / hardware migration).
                            // First registrant keeps the anchor; block the eviction to prevent
                            // endless oscillation where each node evicts the other every gossip round.
                            let canonical_wallet_matches = nodes
                                .values()
                                .find(|n| {
                                    n.masternode
                                        .address
                                        .split(':')
                                        .next()
                                        .unwrap_or(&n.masternode.address)
                                        == canonical_ip
                                })
                                .map(|n| &n.masternode.wallet_address == utxo_address)
                                .unwrap_or(false);
                            if canonical_wallet_matches {
                                let is_local = local_ip
                                    .as_deref()
                                    .map(|l| l == incoming_ip)
                                    .unwrap_or(false);
                                if !is_local {
                                    let spam_key =
                                        format!("stalemate_warn:{}:{}", outpoint_key, incoming_ip);
                                    let last_warned = self
                                        .collateral_migration_times
                                        .get(&spam_key)
                                        .map(|v| *v)
                                        .unwrap_or(0);
                                    if now.saturating_sub(last_warned) >= 300 {
                                        self.collateral_migration_times.insert(spam_key, now);
                                        tracing::warn!(
                                            "🛡️ Dual-claimant stalemate: both {} and {} hold wallet \
                                             matching UTXO {} — canonical {} keeps registration \
                                             (use on-chain MasternodeReg to migrate)",
                                            canonical,
                                            masternode.address,
                                            outpoint_key,
                                            canonical,
                                        );
                                    }
                                    return Err(RegistryError::CollateralAlreadyLocked);
                                }
                            }

                            // The canonical holder's wallet does NOT match the UTXO output
                            // address, but the incoming node's wallet does.  The canonical
                            // holder is a squatter (registered a collateral they don't own)
                            // and must be evicted regardless of their tier.
                            // Tier cannot override UTXO-address proof of ownership.

                            // Legitimate owner proved via UTXO output address — evict squatter
                            tracing::info!(
                                "✅ Collateral ownership verified via UTXO address for {} \
                                 (outpoint {}): evicting squatter {}",
                                masternode.address,
                                outpoint,
                                canonical
                            );
                            // Re-anchor to the legitimate owner
                            let _ = self
                                .db
                                .insert(anchor_db_key.as_bytes(), masternode.address.as_bytes());
                            // Remove the squatter's registry entry
                            nodes.remove(canonical_ip);
                            // Fall through to register the legitimate owner
                        } else {
                            // UTXO address doesn't match — genuine hijack attempt or wrong wallet
                            let spam_key = format!("hijack_warn:{}:{}", outpoint_key, incoming_ip);
                            let last_warned = self
                                .collateral_migration_times
                                .get(&spam_key)
                                .map(|v| *v)
                                .unwrap_or(0);
                            if now.saturating_sub(last_warned) >= 300 {
                                self.collateral_migration_times.insert(spam_key, now);
                                tracing::warn!(
                                    "🛡️ Collateral hijack rejected: {} tried to claim {} \
                                     anchored to {} (UTXO address: {})",
                                    masternode.address,
                                    outpoint,
                                    canonical,
                                    utxo_address
                                );
                            }
                            return Err(RegistryError::CollateralAlreadyLocked);
                        }
                    } else {
                        // Can't look up UTXO address (no UTXO manager or UTXO not found) —
                        // fall back to original first-claim behaviour.
                        let spam_key = format!("hijack_warn:{}:{}", outpoint_key, incoming_ip);
                        let last_warned = self
                            .collateral_migration_times
                            .get(&spam_key)
                            .map(|v| *v)
                            .unwrap_or(0);
                        if now.saturating_sub(last_warned) >= 300 {
                            self.collateral_migration_times.insert(spam_key, now);
                            let existing_is_onchain = nodes
                                .get(canonical_ip)
                                .map(|i| {
                                    matches!(i.registration_source, RegistrationSource::OnChain(_))
                                })
                                .unwrap_or(false);
                            if existing_is_onchain {
                                tracing::warn!(
                                    "🛡️ Collateral hijack rejected: {} tried to claim {} \
                                     already anchored on-chain to {}",
                                    masternode.address,
                                    outpoint,
                                    canonical
                                );
                            } else {
                                tracing::warn!(
                                    "🛡️ Collateral hijack rejected: {} tried to claim {} \
                                     already anchored (first filed) to {}",
                                    masternode.address,
                                    outpoint,
                                    canonical
                                );
                            }
                        }
                        return Err(RegistryError::CollateralAlreadyLocked);
                    }
                }
                // Same address → legitimate reconnect, fall through
            }
            // ── End canonical-anchor check ────────────────────────────────────────

            let mut old_addr_to_remove = None;
            for (addr, info) in nodes.iter() {
                if addr != &masternode.address {
                    if let Some(ref existing_outpoint) = info.masternode.collateral_outpoint {
                        if existing_outpoint == outpoint {
                            old_addr_to_remove = Some(addr.clone());
                            break;
                        }
                    }
                }
            }
            if let Some(ref old_addr) = old_addr_to_remove {
                // Collateral is already claimed by a different IP in memory.
                // Check UTXO ownership first: if the existing holder's wallet_address
                // doesn't match the UTXO output address, they're a squatter — evict them
                // regardless of tier.
                // Use the pre-fetched address (no spawn_blocking inside write lock).
                let utxo_addr = prefetched_utxo_addr.clone();

                if let Some(ref utxo_address) = utxo_addr {
                    if !utxo_address.is_empty() {
                        let old_ip = old_addr.split(':').next().unwrap_or(old_addr);
                        let old_wallet = nodes
                            .get(old_ip)
                            .map(|n| n.masternode.wallet_address.clone())
                            .unwrap_or_default();
                        if old_wallet != *utxo_address && masternode.wallet_address == *utxo_address {
                            // Existing holder's wallet doesn't match UTXO, incoming's does.
                            // Existing holder is the squatter — evict and fall through.
                            tracing::info!(
                                "✅ Evicting in-memory squatter {} (wallet {} ≠ UTXO {}) — {} proved ownership",
                                old_addr, old_wallet, utxo_address, masternode.address
                            );
                            nodes.remove(old_ip);
                        } else if masternode.wallet_address != *utxo_address && !old_wallet.is_empty() {
                            // Incoming node's wallet doesn't match UTXO — they're the squatter
                            tracing::warn!(
                                "🛡️ Collateral squatter rejected: {} wallet {} doesn't match UTXO {} (registered to {})",
                                masternode.address, masternode.wallet_address, utxo_address, old_addr
                            );
                            return Err(RegistryError::CollateralAlreadyLocked);
                        }
                        // If both match or UTXO check is inconclusive, fall through to tier rules
                    }
                }
            }
            if let Some(old_addr) = old_addr_to_remove {

                // Even a Free-tier incoming claim must not displace a paid-tier holder.
                // Attackers exploit the Free-tier migration path to re-steal collateral
                // that was just reclaimed by a legitimate Bronze/Silver/Gold node via
                // Tier-2 eviction (which does not set a canonical anchor).
                let existing_tier = nodes
                    .get(&old_addr)
                    .map(|n| n.masternode.tier)
                    .unwrap_or(crate::types::MasternodeTier::Free);
                if existing_tier != crate::types::MasternodeTier::Free {
                    tracing::warn!(
                        "🛡️ Free-tier claim rejected: {} tried to take {} from paid-tier {} \
                         — on-chain MasternodeReg required",
                        masternode.address,
                        outpoint,
                        old_addr
                    );
                    return Err(RegistryError::CollateralAlreadyLocked);
                }

                // Block migration whose *destination* IP is already held by an on-chain node.
                // A gossip-based Free-tier migration cannot displace a node whose collateral
                // is confirmed on-chain — that is the collateral-churn / IP-squatting attack
                // where an attacker migrates from one of their own IPs to a legitimate node's IP.
                if let Some(dest_info) = nodes.get(&masternode.address) {
                    if matches!(
                        dest_info.registration_source,
                        RegistrationSource::OnChain(_)
                    ) {
                        tracing::warn!(
                            "🛡️ [Collateral-Churn] Migration from {} to on-chain node {} \
                             blocked (incoming outpoint: {})",
                            old_addr,
                            masternode.address,
                            outpoint,
                        );
                        return Err(RegistryError::CollateralAlreadyLocked);
                    }
                }

                // Free tier: no collateral to protect, allow migration with cooldown
                // Never let a remote peer steal the local masternode's collateral
                let old_ip = old_addr.split(':').next().unwrap_or(&old_addr);
                if local_ip.as_deref() == Some(old_ip) {
                    tracing::warn!(
                        "🛡️ Rejected collateral migration: remote {} tried to claim local collateral {}",
                        masternode.address,
                        outpoint
                    );
                    return Err(RegistryError::InvalidCollateral);
                }

                // Post-eviction lockout: if a V4-proof eviction recently removed a squatter
                // from this outpoint, block ALL free-tier re-migrations to it for 10 minutes.
                // This closes the oscillation loop (AV14) where an attacker immediately
                // re-squats under a confederate IP after being evicted by the legitimate owner.
                const POST_EVICTION_LOCKOUT_SECS: u64 = 600;
                if let Some(evicted_at) = self.post_eviction_lockout.get(&outpoint_key) {
                    let elapsed = now.saturating_sub(*evicted_at);
                    if elapsed < POST_EVICTION_LOCKOUT_SECS {
                        let remaining = POST_EVICTION_LOCKOUT_SECS - elapsed;
                        tracing::warn!(
                            "🛡️ Post-eviction lockout: {} tried to re-squat {} \
                             via free-tier migration {}s after V4 eviction \
                             ({}s remaining — use V4 proof to override)",
                            masternode.address,
                            outpoint,
                            elapsed,
                            remaining
                        );
                        return Err(RegistryError::InvalidCollateral);
                    }
                }

                // Cooldown: refuse to migrate the same outpoint more than once per 300 s.
                // Raised from 60 s to 300 s to reduce IP churn / cycling attack frequency (AV3).
                const MIGRATION_COOLDOWN_SECS: u64 = 300;
                if let Some(last_migrated) = self.collateral_migration_times.get(&outpoint_key) {
                    let elapsed = now.saturating_sub(*last_migrated);
                    if elapsed < MIGRATION_COOLDOWN_SECS {
                        let remaining = MIGRATION_COOLDOWN_SECS - elapsed;
                        if elapsed < 2 {
                            tracing::debug!(
                                "Free-tier collateral {} cooldown ({}s remaining) — \
                                 ignoring claim from {}",
                                outpoint,
                                remaining,
                                masternode.address
                            );
                        }
                        return Err(RegistryError::InvalidCollateral);
                    }
                }

                // Back-and-forth IP cycling detection (AV3): if the incoming IP matches
                // the IP this outpoint just migrated FROM, reject it as a cycling attack.
                // With a 300 s cooldown, legitimate operators can still change IPs;
                // attackers who flip A→B→A every ~300 s are blocked for 10 minutes.
                {
                    const CYCLING_LOCKOUT_SECS: u64 = 600;
                    let incoming_ip = masternode
                        .address
                        .split(':')
                        .next()
                        .unwrap_or(&masternode.address);
                    if let Some(prev_from) = self.collateral_migration_from.get(&outpoint_key) {
                        if prev_from.as_str() == incoming_ip {
                            if let Some(last_migrated) =
                                self.collateral_migration_times.get(&outpoint_key)
                            {
                                if now.saturating_sub(*last_migrated) < CYCLING_LOCKOUT_SECS {
                                    tracing::warn!(
                                        "🛡️ IP cycling rejected (AV3): {} tried to move {} \
                                         back to {} (came from there {}s ago, lockout {}s)",
                                        masternode.address,
                                        outpoint,
                                        incoming_ip,
                                        now.saturating_sub(*last_migrated),
                                        CYCLING_LOCKOUT_SECS,
                                    );
                                    return Err(RegistryError::IpCyclingRejected);
                                }
                            }
                        }
                    }
                }

                // AV26: Pool rotation detection — limit migration frequency per outpoint.
                // Attackers cycle A→B→C→D→A to evade the simple back-and-forth check.
                // We reject if this outpoint has migrated >= 3 times within the last 30 min.
                {
                    const MAX_MIGRATIONS_PER_WINDOW: u32 = 3;
                    const MIGRATION_WINDOW_SECS: u64 = 1800; // 30 minutes
                    let (count, window_start) = self
                        .collateral_migration_counts
                        .get(&outpoint_key)
                        .map(|v| *v)
                        .unwrap_or((0, now));
                    let elapsed = now.saturating_sub(window_start);
                    let (effective_count, effective_start) = if elapsed >= MIGRATION_WINDOW_SECS {
                        (0, now) // Window expired — reset
                    } else {
                        (count, window_start)
                    };
                    if effective_count >= MAX_MIGRATIONS_PER_WINDOW {
                        tracing::warn!(
                            "🛡️ [AV26] Migration flood rejected: {} tried to move {} \
                             ({} migrations in {}s, max {} per {}s window)",
                            masternode.address,
                            outpoint,
                            effective_count,
                            elapsed,
                            MAX_MIGRATIONS_PER_WINDOW,
                            MIGRATION_WINDOW_SECS,
                        );
                        return Err(RegistryError::InvalidCollateral);
                    }
                    self.collateral_migration_counts
                        .insert(outpoint_key.clone(), (effective_count + 1, effective_start));
                }

                // Record the source IP before updating the migration timestamp so that
                // the next migration attempt can be checked for back-tracking.
                {
                    let old_ip = old_addr.split(':').next().unwrap_or(&old_addr).to_string();
                    self.collateral_migration_from
                        .insert(outpoint_key.clone(), old_ip);
                }

                self.collateral_migration_times
                    .insert(outpoint_key.clone(), now);

                tracing::info!(
                    "🔄 Free-tier IP migration: {} moving from {} to {}",
                    outpoint,
                    old_addr,
                    masternode.address
                );
                nodes.remove(&old_addr);
                let key = format!("masternode:{}", old_addr).into_bytes();
                self.sled_remove_bg(key);
            } else if anchor_addr.is_none() && masternode.tier != crate::types::MasternodeTier::Free
            {
                // Gossip does NOT set collateral anchors for paid tiers.
                // Only a confirmed on-chain MasternodeReg tx (which requires a signature
                // from the collateral owner's private key) may anchor a paid-tier outpoint.
                // This prevents an attacker from gossip-squatting a collateral UTXO before
                // the real owner submits their registration transaction.
                tracing::debug!(
                    "⚠️ Gossip claim for un-anchored paid-tier collateral {} from {} — \
                     ignoring (on-chain MasternodeReg required to anchor)",
                    outpoint,
                    masternode.address
                );
            }
        }

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
                // If collateral outpoint changed, queue the old one for unlock.
                // Never let None overwrite an existing Some — gossip/exchange data
                // may lack the outpoint even though the node has valid collateral.
                let old_outpoint = existing.masternode.collateral_outpoint.clone();
                let new_outpoint = masternode.collateral_outpoint.clone();

                // Block collateral-churn: remote gossip cannot replace the on-chain outpoint
                // of an on-chain registered node. Only a new on-chain MasternodeReg tx may
                // do that. This closes the attack where a remote peer floods V4 announces
                // claiming a legitimate node's IP with different collateral outpoints,
                // causing the real on-chain collateral to be queued for unlock.
                //
                // V3 announcements (old code, no collateral_outpoint field) arrive with
                // new_outpoint = None. These are NOT an attack — the node is simply
                // re-announcing without proof. The None → preserve-existing path at line
                // 1149 handles them correctly; we must not reject them here.
                if old_outpoint != new_outpoint
                    && old_outpoint.is_some()
                    && new_outpoint.is_some()
                    && matches!(existing.registration_source, RegistrationSource::OnChain(_))
                {
                    static CHURN_WARN: std::sync::OnceLock<
                        dashmap::DashMap<String, std::time::Instant>,
                    > = std::sync::OnceLock::new();
                    let warn_map = CHURN_WARN.get_or_init(dashmap::DashMap::new);
                    let needs_log = warn_map
                        .get(&masternode.address)
                        .map(|t| t.elapsed().as_secs() >= 60)
                        .unwrap_or(true);
                    if needs_log {
                        warn_map.insert(masternode.address.clone(), std::time::Instant::now());
                        tracing::warn!(
                            "🛡️ [Collateral-Churn] Blocked outpoint replacement for on-chain \
                             node {} (on-chain: {}, rejected: {})",
                            masternode.address,
                            old_outpoint
                                .as_ref()
                                .map(|o| o.to_string())
                                .unwrap_or_default(),
                            new_outpoint
                                .as_ref()
                                .map(|o| o.to_string())
                                .unwrap_or_default(),
                        );
                    }
                    return Err(RegistryError::CollateralAlreadyLocked);
                }

                let effective_outpoint = if new_outpoint.is_none() && old_outpoint.is_some() {
                    // Preserve existing outpoint — incoming data is incomplete
                    old_outpoint.clone()
                } else {
                    if old_outpoint != new_outpoint {
                        if let Some(old_op) = old_outpoint {
                            tracing::info!(
                                "🔓 Collateral changed for {} — queuing old outpoint for unlock",
                                masternode.address
                            );
                            self.pending_collateral_unlocks.lock().push(old_op);
                        }
                        // Queue the new collateral for immediate locking
                        if let Some(ref new_op) = new_outpoint {
                            let lock_height = self
                                .current_height
                                .load(std::sync::atomic::Ordering::Relaxed);
                            self.pending_collateral_locks.lock().push((
                                new_op.clone(),
                                masternode.address.clone(),
                                lock_height,
                                masternode.tier.collateral(),
                            ));
                        }
                    }
                    new_outpoint
                };

                // Update tier and collateral info on re-registration
                existing.masternode.tier = masternode.tier;
                if masternode.collateral > 0 {
                    existing.masternode.collateral = masternode.collateral;
                }
                existing.masternode.collateral_outpoint = effective_outpoint;
                existing.masternode.public_key = masternode.public_key;
                // Sync wallet_address (used by block reward logic) with reward_address from config
                existing.masternode.wallet_address = masternode.wallet_address.clone();
                existing.reward_address = reward_address.clone();
            }

            if !existing.is_active && should_activate {
                existing.is_active = true;
                existing.uptime_start = now;
                debug!(
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

            // Reset any collateral miss count on re-registration.
            // A live connection proves the node is back — don't carry over stale
            // miss counts that were accumulated while we were syncing (when the
            // collateral UTXO wasn't in our local set yet).
            self.collateral_miss_counts.remove(&masternode.address);

            // Update on disk
            self.store_masternode(&masternode.address, existing)?;

            self.rebuild_node_caches(&nodes);
            return Ok(());
        }

        // AV25: Per-/24 subnet cap for Free-tier nodes.
        // Attackers flood the registry from one subnet; cap each /24 at 5 Free-tier nodes.
        // Paid tiers are not capped — they have on-chain collateral as proof-of-stake.
        const MAX_FREE_TIER_PER_SUBNET: u32 = 5;
        if masternode.tier == crate::types::MasternodeTier::Free {
            let subnet = Self::free_tier_subnet(&masternode.address);
            let count = self
                .free_tier_subnet_counts
                .get(&subnet)
                .map(|c| *c)
                .unwrap_or(0);
            if count >= MAX_FREE_TIER_PER_SUBNET {
                // Rate-limit this WARN to at most once per 30s per subnet.
                // During AV3 cycling windows, 100+ nodes burst in simultaneously
                // after the old 5 disconnect — logging each one creates WARN floods.
                static SUBNET_REJECT_LIMITER: std::sync::OnceLock<
                    dashmap::DashMap<String, std::time::Instant>,
                > = std::sync::OnceLock::new();
                let limiter = SUBNET_REJECT_LIMITER.get_or_init(dashmap::DashMap::new);
                let needs_log = limiter
                    .get(&subnet)
                    .map(|t| t.elapsed().as_secs() >= 30)
                    .unwrap_or(true);
                if needs_log {
                    limiter.insert(subnet.clone(), std::time::Instant::now());
                    tracing::warn!(
                        "🚫 [AV25] Free-tier registration rejected: /24 {} already has {} nodes (max {})",
                        subnet,
                        count,
                        MAX_FREE_TIER_PER_SUBNET
                    );
                }
                return Err(RegistryError::InvalidCollateral);
            }
        }

        let current_h = self
            .current_height
            .load(std::sync::atomic::Ordering::Relaxed);
        let info = MasternodeInfo {
            masternode: masternode.clone(),
            reward_address: reward_address.clone(),
            uptime_start: now,
            total_uptime: 0,
            is_active: should_activate, // Only active if explicitly activated (true for connections, false for gossip)
            daemon_started_at: 0,
            last_reward_height: 0,
            blocks_without_reward: 0,
            registration_height: current_h, // Anti-sybil: track when node first appeared
            registration_source: RegistrationSource::Handshake,
            operator_pubkey: None, // Set only for on-chain registrations (two-key model)
            peer_reports: Arc::new(DashMap::new()),
            is_publicly_reachable: false, // Must pass reachability probe before earning rewards
            reachability_checked_at: 0,
            first_seen_at: now,
            last_seen_at: if should_activate { now } else { 0 },
        };

        // Persist to disk
        self.store_masternode(&masternode.address, &info)?;

        // Clear any stale miss count for this address (may exist from a prior
        // registration cycle that was cleaned up during sync).
        self.collateral_miss_counts.remove(&masternode.address);

        nodes.insert(masternode.address.clone(), info);
        let total_masternodes = nodes.len();

        // AV25: Track Free-tier subnet count for the registration cap.
        if masternode.tier == crate::types::MasternodeTier::Free {
            let subnet = Self::free_tier_subnet(&masternode.address);
            *self.free_tier_subnet_counts.entry(subnet).or_insert(0) += 1;
        }

        // Write a canonical anchor for paid-tier nodes on first registration.
        // Without this, the collateral has no anchor in sled, and a later Free-tier
        // gossip announcement with the same outpoint could migrate it via the
        // old_addr_to_remove path (which only checks the in-memory registry, not sled).
        // The anchor ensures all future claims must pass the canonical-anchor check.
        if masternode.tier != crate::types::MasternodeTier::Free {
            if let Some(ref outpoint) = masternode.collateral_outpoint {
                let outpoint_key = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
                let anchor_key = format!("collateral_anchor:{}", outpoint_key);
                if self.db.get(anchor_key.as_bytes()).ok().flatten().is_none() {
                    self.sled_insert_bg(
                        anchor_key.into_bytes(),
                        masternode.address.as_bytes().to_vec(),
                    );
                    tracing::debug!(
                        "🔒 Set collateral anchor {} → {}",
                        outpoint_key,
                        masternode.address
                    );
                }
            }
        }

        debug!(
            "✅ Registered masternode {} (total: {}) - NEW - Tier: {:?}, Reward address: {}, Active at timestamp: {}",
            masternode.address,
            total_masternodes,
            masternode.tier,
            reward_address,
            now
        );
        self.rebuild_node_caches(&nodes);
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

    /// Get active masternodes eligible for VRF sortition (excludes immature Free-tier nodes).
    /// Paid tiers are always eligible; Free-tier must pass the maturity gate.
    pub async fn get_vrf_eligible(&self, current_height: u64) -> Vec<(Masternode, String)> {
        let masternodes = self.masternodes.read().await;
        masternodes
            .values()
            .filter(|info| {
                info.is_active && Self::is_mature_for_sortition(info, current_height, self.network)
            })
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
        // SAFETY: Never remove or deactivate the local masternode on a peer
        // disconnect event. The local node is always running and its entry
        // must persist.
        let is_local = self
            .local_masternode_address
            .read()
            .await
            .as_ref()
            .map(|a| {
                let local_ip = a.split(':').next().unwrap_or(a);
                let addr_ip = address.split(':').next().unwrap_or(address);
                local_ip == addr_ip
            })
            .unwrap_or(false);

        if is_local {
            tracing::debug!(
                "🛡️ Ignoring disconnect event for local masternode {}",
                address
            );
            return Ok(());
        }

        let now = Self::now();

        // Dedup guard: multiple tokio tasks can call mark_inactive_on_disconnect for the
        // same IP within the same second (e.g., TCP close + timeout racing).  If the IP
        // was already processed within the last 5 seconds, skip this call silently.
        // We check/set the cooldown map BEFORE taking the write lock so the fast path
        // (already-removed) does zero lock work.
        if let Some(removed_at) = self.free_tier_reconnect_cooldown.get(address) {
            if now.saturating_sub(*removed_at) < 5 {
                tracing::debug!(
                    "🔁 Skipping duplicate disconnect for {} (already processed {}s ago)",
                    address,
                    now.saturating_sub(*removed_at)
                );
                return Ok(());
            }
        }

        let mut masternodes = self.masternodes.write().await;

        // Read properties before mutating to avoid borrow conflicts.
        let (is_handshake, is_active) = {
            let info = masternodes.get(address).ok_or(RegistryError::NotFound)?;
            (
                info.registration_source == RegistrationSource::Handshake,
                info.is_active,
            )
        };

        // Check if node has collateral (paid tier) — if so, never remove on disconnect
        // regardless of registration source. Gossip-announced Bronze+ nodes use Handshake
        // source but should persist just like on-chain registrations.
        let has_collateral = {
            masternodes
                .get(address)
                .and_then(|i| i.masternode.collateral_outpoint.as_ref())
                .is_some()
        };

        if is_handshake && !has_collateral {
            // Free-tier nodes: keep in registry with last_seen_at set so the
            // ELIGIBILITY_GRACE_SECS window and clean_stale_free_tier_nodes() work correctly.
            // Previously we removed them immediately, but that causes reward gaps when a
            // legitimate node briefly disconnects (e.g., under a targeted disconnect attack).
            {
                let info = masternodes.get_mut(address).expect("checked above");
                info.is_active = false;
                info.last_seen_at = now;
                if info.uptime_start > 0 {
                    info.total_uptime += now - info.uptime_start;
                }
            }
            self.rebuild_node_caches(&masternodes);
            drop(masternodes);

            // AV25: Decrement the per-subnet Free-tier count when the node goes inactive.
            let subnet = Self::free_tier_subnet(address);
            if let Some(mut c) = self.free_tier_subnet_counts.get_mut(&subnet) {
                *c = c.saturating_sub(1);
            }

            // AV3: Record removal timestamp so rapid re-registration is blocked for 30s.
            self.free_tier_reconnect_cooldown
                .insert(address.to_string(), now);

            tracing::debug!(
                "🔌 Masternode {} marked inactive on disconnect (Free-tier, kept in registry for grace period)",
                address
            );

            // Broadcast inactive so peers update their active sets immediately.
            if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
                let _ = tx.send(
                    crate::network::message::NetworkMessage::MasternodeInactive {
                        address: address.to_string(),
                        timestamp: now,
                    },
                );
            }
        } else if is_active {
            // On-chain registered nodes (Bronze+) paid collateral and may reconnect.
            // Mark inactive so they are excluded from vote weight until they return.
            {
                let info = masternodes.get_mut(address).expect("checked above");
                info.is_active = false;
                info.last_seen_at = now;
                if info.uptime_start > 0 {
                    info.total_uptime += now - info.uptime_start;
                }

                warn!(
                    "⚠️  Masternode {} marked inactive (connection lost) - broadcasting to network",
                    address
                );

                // Persist while still holding the write lock (fast — bg channel).
                self.store_masternode(address, info)?;
            }
            self.rebuild_node_caches(&masternodes);

            // Drop the write lock BEFORE awaiting on broadcast_tx so we don't hold
            // masternodes.write() across an async boundary.
            drop(masternodes);

            // Fire the PHASE3 priority-reconnect signal so the reconnect loop wakes
            // immediately instead of waiting up to 30 s for the next PHASE3 tick.
            self.priority_reconnect_notify.notify_one();

            if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
                let _ = tx.send(
                    crate::network::message::NetworkMessage::MasternodeInactive {
                        address: address.to_string(),
                        timestamp: now,
                    },
                );
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn unregister(&self, address: &str) -> Result<Option<MasternodeInfo>, RegistryError> {
        let mut nodes = self.masternodes.write().await;

        if !nodes.contains_key(address) {
            return Err(RegistryError::NotFound);
        }

        let removed = nodes.remove(address);

        // Queue disk removals via background writer (no sled I/O under write lock).
        self.sled_remove_bg(format!("masternode:{}", address).into_bytes());

        if let Some(ref info) = removed {
            if let Some(ref op) = info.masternode.collateral_outpoint {
                let outpoint_str = format!("{}:{}", hex::encode(op.txid), op.vout);
                let anchor_key = format!("collateral_anchor:{}", outpoint_str);
                self.sled_remove_bg(anchor_key.into_bytes());
                debug!(
                    "🔓 Removed collateral anchor {} on masternode deregistration",
                    outpoint_str
                );
            }
        }

        self.rebuild_node_caches(&nodes);
        Ok(removed)
    }
    pub async fn find_by_reward_address(
        &self,
        reward_addr: &str,
    ) -> Option<(String, MasternodeInfo)> {
        let nodes = self.masternodes.read().await;
        nodes
            .iter()
            .find(|(_, info)| info.reward_address == reward_addr)
            .map(|(addr, info)| (addr.clone(), info.clone()))
    }

    /// Find which network address currently holds the given collateral outpoint.
    pub async fn find_holder_of_outpoint(
        &self,
        outpoint: &crate::types::OutPoint,
    ) -> Option<String> {
        let nodes = self.masternodes.read().await;
        nodes
            .iter()
            .find(|(_, info)| info.masternode.collateral_outpoint.as_ref() == Some(outpoint))
            .map(|(addr, _)| addr.clone())
    }

    #[allow(dead_code)]
    pub async fn get(&self, address: &str) -> Option<MasternodeInfo> {
        self.masternodes.read().await.get(address).cloned()
    }

    /// Returns the timestamp when this daemon started (for inclusion in announcements)
    pub fn get_started_at(&self) -> u64 {
        self.started_at
    }

    /// Update a masternode's daemon_started_at from an announcement
    pub async fn update_daemon_started_at(&self, address: &str, started_at: u64) {
        if started_at == 0 {
            return;
        }
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(address) {
            if info.daemon_started_at != started_at {
                info.daemon_started_at = started_at;
                let _ = self.store_masternode(address, info);
            }
        }
    }

    pub async fn list_all(&self) -> Vec<MasternodeInfo> {
        self.masternodes.read().await.values().cloned().collect()
    }

    /// Look up the tier for a wallet (reward) address.
    /// Used by reward validation to classify entries in block.masternode_rewards
    /// without needing to know which masternodes were active at production time.
    /// Returns None if the wallet address is not in the current registry.
    pub async fn tier_for_wallet(
        &self,
        wallet_address: &str,
    ) -> Option<crate::types::MasternodeTier> {
        // When a wallet is registered at multiple IPs and/or multiple tiers (e.g. an
        // operator that runs both a Silver node and a Free node with the same wallet),
        // return the HIGHEST tier found.  This makes block validation deterministic
        // regardless of HashMap iteration order, preventing per-tier pool mismatches
        // caused by non-deterministic tier classification across different nodes.
        // Discriminant values: Gold=100000 > Silver=10000 > Bronze=1000 > Free=0
        //
        // Match against reward_address (if set) OR wallet_address, mirroring the
        // block-production logic.  A mismatch here is what produces "unknown recipient"
        // errors that incorrectly reject valid blocks (fork bug).
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| {
                info.masternode.wallet_address == wallet_address
                    || (!info.reward_address.is_empty() && info.reward_address == wallet_address)
            })
            .map(|info| info.masternode.tier)
            .max_by_key(|tier| *tier as u64)
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

    /// Get Free-tier masternodes eligible for the participation pool.
    /// All on-chain registered Free nodes are eligible immediately — no maturity gate.
    pub async fn get_eligible_free_nodes(&self, current_height: u64) -> Vec<MasternodeInfo> {
        let maturity_required: u64 = 0;
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| {
                info.is_active
                    && matches!(info.masternode.tier, crate::types::MasternodeTier::Free)
                    && (info.registration_height == 0
                        || current_height.saturating_sub(info.registration_height)
                            >= maturity_required)
            })
            .cloned()
            .collect()
    }

    /// Get ALL active masternodes eligible for the weighted reward pool (§10.4).
    /// Paid tiers (Bronze/Silver/Gold) are always eligible.
    /// Free tier requires maturity gate on mainnet.
    /// All tiers require bidirectional network reachability: nodes behind NAT/firewall
    /// that cannot accept inbound connections are excluded from block rewards.
    ///
    /// Two-pass fallback: if strict filtering yields < MIN_PAID_RECIPIENTS (3) nodes,
    /// the pool is too small to produce a valid block (which requires ≥3 distinct
    /// non-zero recipients). In that case we widen the net progressively:
    ///   Pass 1 — reachability + maturity (normal)
    ///   Pass 2 — maturity only (no reachability requirement)
    ///   Pass 3 — active only (no reachability or maturity requirement)
    /// This prevents a deadlock on young chains or heavily-NATted networks while
    /// the ≥3-recipient rule still guards against single-node reward monopolization.
    pub async fn get_eligible_pool_nodes(&self, current_height: u64) -> Vec<MasternodeInfo> {
        const MIN_VIABLE_POOL: usize = 3;
        let maturity_required: u64 = 0;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let nodes = self.masternodes.read().await;

        let is_mature = |info: &MasternodeInfo| -> bool {
            !matches!(info.masternode.tier, crate::types::MasternodeTier::Free)
                || info.registration_height == 0
                || current_height.saturating_sub(info.registration_height) >= maturity_required
        };
        let is_reachable = |info: &MasternodeInfo| -> bool {
            let within_grace_period = info.first_seen_at > 0
                && now.saturating_sub(info.first_seen_at) < REACHABILITY_GRACE_PERIOD_SECS;
            within_grace_period || info.is_publicly_reachable
        };

        // Free-tier activity check uses a 15-minute window (1.5× the 10-minute block
        // interval) instead of the standard 5-minute gossip-report expiry.  Gossip
        // reports expire at 5 min but blocks arrive every 10 min, so a free node
        // reported just before one block can drop below the ≥3-reports threshold by
        // the next block — producing a one-node-at-a-time rotation instead of the
        // intended split among all connected free nodes.  The extended window keeps
        // all recently-seen free nodes simultaneously eligible so the 8 TIME pool is
        // divided among them rather than awarded in full to a single rotating winner.
        let free_active_window_secs: u64 = 900; // 15 minutes
        let is_free_active = |info: &MasternodeInfo| -> bool {
            if info.is_active {
                return true; // Already active by normal gossip criteria
            }
            // Secondary check: any peer reported this node within the extended window.
            info.peer_reports
                .iter()
                .any(|entry| now.saturating_sub(*entry.value()) < free_active_window_secs)
        };

        // Grace window for paid-tier nodes: a recently-disconnected Bronze/Silver/Gold
        // node stays eligible for rewards for 90 seconds after the connection dropped.
        // This prevents a targeted "disconnect to steal reward" attack from succeeding
        // if the victim reconnects within one block slot.
        const ELIGIBILITY_GRACE_SECS: u64 = 90;
        let is_within_grace = |info: &MasternodeInfo| -> bool {
            !matches!(info.masternode.tier, crate::types::MasternodeTier::Free)
                && info.last_seen_at > 0
                && now.saturating_sub(info.last_seen_at) < ELIGIBILITY_GRACE_SECS
        };

        // Pass 1: full filters (normal path).
        // Free-tier uses the extended activity window; paid tiers use is_active or grace window.
        let pass1: Vec<MasternodeInfo> = nodes
            .values()
            .filter(|info| {
                let active = if matches!(info.masternode.tier, crate::types::MasternodeTier::Free) {
                    is_free_active(info)
                } else {
                    info.is_active || is_within_grace(info)
                };
                active && is_reachable(info) && is_mature(info)
            })
            .cloned()
            .collect();
        if pass1.len() >= MIN_VIABLE_POOL {
            return pass1;
        }

        // Pass 2: drop reachability requirement (handles NAT-heavy networks).
        let pass2: Vec<MasternodeInfo> = nodes
            .values()
            .filter(|info| (info.is_active || is_within_grace(info)) && is_mature(info))
            .cloned()
            .collect();
        if pass2.len() >= MIN_VIABLE_POOL {
            tracing::debug!(
                "Eligible pool fallback (pass 2 — no reachability): {} nodes (pass 1 had {})",
                pass2.len(),
                pass1.len()
            );
            return pass2;
        }

        // Pass 3: drop maturity too (handles young/bootstrapping chains where no node
        // has been present for FREE_MATURITY_BLOCKS yet). The ≥3-recipient rule
        // in the block validator still prevents single-node reward monopolization.
        let pass3: Vec<MasternodeInfo> = nodes
            .values()
            .filter(|info| info.is_active || is_within_grace(info))
            .cloned()
            .collect();
        if pass3.len() > pass1.len() {
            tracing::debug!(
                "Eligible pool fallback (pass 3 — active only): {} nodes (pass 1 had {})",
                pass3.len(),
                pass1.len()
            );
        }
        pass3
    }

    /// Check if a Free-tier masternode is mature enough for VRF sortition.
    /// Paid tiers are always mature (collateral is their skin in the game).
    pub fn is_mature_for_sortition(
        info: &MasternodeInfo,
        current_height: u64,
        network: crate::NetworkType,
    ) -> bool {
        if !matches!(info.masternode.tier, crate::types::MasternodeTier::Free) {
            return true; // Paid tiers always eligible
        }
        // No maturity gate — all on-chain registered nodes are immediately eligible.
        let _ = (network, current_height);
        true
    }

    /// Set registration height for a masternode (called once when first block is seen)
    pub async fn set_registration_height(&self, address: &str, height: u64) {
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(address) {
            if info.registration_height == 0 {
                info.registration_height = height;
                self.store_masternode(address, info).ok();
            }
        }
    }

    /// Check if a masternode at the given address is mature for VRF sortition.
    /// Returns true for paid tiers (always eligible) and mature Free-tier nodes.
    /// Returns true if address not found (conservative: don't block unknown nodes).
    pub async fn is_address_vrf_eligible(&self, address: &str, current_height: u64) -> bool {
        let nodes = self.masternodes.read().await;
        match nodes.get(address) {
            Some(info) => Self::is_mature_for_sortition(info, current_height, self.network),
            None => true,
        }
    }

    /// Atomically claim a reachability probe slot for `address`.
    ///
    /// Returns `true` if the caller should proceed with a probe (and stamps
    /// `reachability_checked_at` to now to prevent duplicate probes).
    /// Returns `false` if a probe was performed recently (within
    /// `REACHABILITY_RECHECK_SECS`) and no new probe is needed yet.
    pub async fn try_claim_reachability_probe(&self, address: &str) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(address) {
            if info.reachability_checked_at > 0
                && now.saturating_sub(info.reachability_checked_at) < REACHABILITY_RECHECK_SECS
            {
                return false; // recently probed, skip
            }
            info.reachability_checked_at = now; // claim the slot
            true
        } else {
            false // unknown node, nothing to probe
        }
    }

    /// Mark a masternode as publicly reachable (or not) based on a TCP probe result.
    /// Called after an outbound connection succeeds (always reachable) or after a
    /// reverse-probe of an inbound-only connection.
    pub async fn set_publicly_reachable(&self, address: &str, reachable: bool) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(address) {
            let changed = info.is_publicly_reachable != reachable;
            info.is_publicly_reachable = reachable;
            info.reachability_checked_at = now;
            if changed {
                if reachable {
                    info!(
                        "🌐 Masternode {} is now publicly reachable — eligible for rewards",
                        address
                    );
                } else {
                    warn!(
                        "⚠️  Masternode {} failed reachability probe — not publicly reachable. \
                         Excluded from block rewards until bidirectional connectivity is confirmed.",
                        address
                    );
                }
                self.store_masternode(address, info).ok();
            }
        }
    }

    /// Returns addresses of masternodes that need a reachability probe:
    ///  - Not yet probed, OR
    ///  - Last probe was more than REACHABILITY_RECHECK_SECS ago.
    ///
    /// Only returns nodes that are currently active (inactive nodes aren't earning anyway).
    pub async fn get_nodes_needing_reachability_probe(&self) -> Vec<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let nodes = self.masternodes.read().await;
        nodes
            .values()
            .filter(|info| {
                info.is_active
                    && (info.reachability_checked_at == 0
                        || now.saturating_sub(info.reachability_checked_at)
                            >= REACHABILITY_RECHECK_SECS)
            })
            .map(|info| info.masternode.address.clone())
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

        // BOOTSTRAP MODE: Block 1 (height 0 → 1) uses ALL registered masternodes
        // since there is no previous bitmap yet.
        if current_height == 0 {
            let all_masternodes: Vec<MasternodeInfo> = self.list_all().await;
            static LAST_BLOCK1_LOG: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = chrono::Utc::now().timestamp();
            let last = LAST_BLOCK1_LOG.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last >= 30 {
                LAST_BLOCK1_LOG.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    "💰 Block 1 (first block after genesis): using {} registered masternodes for bootstrap (including inactive, no bitmap yet)",
                    all_masternodes.len()
                );
            }
            return all_masternodes;
        }

        // Get previous block to see who participated.
        // The bitmap in each block now includes BOTH direct voters AND gossip-active
        // masternodes, so all online nodes (even those not directly connected to the
        // previous producer) appear here and qualify for rewards this block.
        let prev_block = match blockchain.get_block_by_height(current_height).await {
            Ok(block) => block,
            Err(e) => {
                tracing::warn!(
                    "⚠️  Failed to get previous block {} for reward calculation: {}",
                    current_height,
                    e
                );
                return self.get_active_masternodes().await;
            }
        };

        // Collect addresses that participated (producer + bitmap voters)
        let mut participants = std::collections::HashSet::new();

        if !prev_block.header.leader.is_empty() {
            participants.insert(prev_block.header.leader.clone());
        }

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

        let masternodes = self.masternodes.read().await;
        let eligible: Vec<MasternodeInfo> = masternodes
            .values()
            .filter(|mn| participants.contains(&mn.masternode.address))
            .cloned()
            .collect();

        tracing::debug!(
            "💰 Block {}: {} masternodes eligible for rewards (in previous block {} bitmap)",
            current_height + 1,
            eligible.len(),
            current_height
        );

        // CRITICAL SAFETY: refuse block production if too few masternodes.
        if eligible.len() < 3 {
            let active = self.get_active_masternodes().await;
            if active.len() < 3 {
                use std::sync::atomic::{AtomicI64, Ordering as AtomOrd};
                static LAST_FORK_WARN: AtomicI64 = AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last = LAST_FORK_WARN.load(AtomOrd::Relaxed);
                if now_secs - last >= 60 {
                    LAST_FORK_WARN.store(now_secs, AtomOrd::Relaxed);
                    tracing::error!(
                        "🛡️ FORK PREVENTION: Only {} active masternodes (minimum 3 required) - refusing block production",
                        active.len()
                    );
                }
                return Vec::new();
            }
            tracing::warn!(
                "⚠️ Bitmap had {} participants, falling back to {} active masternodes",
                eligible.len(),
                active.len()
            );
            return active;
        }

        eligible
    }

    /// Count all registered masternodes (not just active ones)
    /// Used during genesis and bootstrap when heartbeat requirements are relaxed
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
    /// Bitmap format: 1 bit per masternode, ordered by ascending `slot_id`.
    ///
    /// `slot_id` is assigned permanently at registration and never changes, so
    /// the bitmap position of each node is stable across all nodes on any chain
    /// state — eliminating the AV6 position-drift attack.
    ///
    /// Bit = 1: masternode voted on this block (active participant)
    /// Bit = 0: masternode did not vote (inactive or offline)
    pub async fn create_active_bitmap_from_voters(&self, voters: &[String]) -> (Vec<u8>, usize) {
        let masternodes = self.masternodes.read().await;

        // Sort by slot_id (permanent, chain-derived — no drift).
        // Nodes with valid slot_ids sort first; unregistered nodes (slot_id == u32::MAX)
        // fall at the end, ordered deterministically by address to prevent position drift.
        // Including unregistered nodes ensures the bitmap captures all active participants
        // even when on-chain registration TXs are pending confirmation.
        let mut sorted_mns: Vec<MasternodeInfo> = masternodes.values().cloned().collect();
        sorted_mns.sort_by(|a, b| {
            a.masternode
                .slot_id
                .cmp(&b.masternode.slot_id)
                .then_with(|| a.masternode.address.cmp(&b.masternode.address))
        });

        if sorted_mns.is_empty() {
            return (vec![], 0);
        }

        let voter_set: std::collections::HashSet<String> = voters.iter().cloned().collect();

        let num_bits = sorted_mns.len();
        let num_bytes = num_bits.div_ceil(8);
        let mut bitmap = vec![0u8; num_bytes];

        let mut active_count = 0;
        for (i, mn) in sorted_mns.iter().enumerate() {
            if voter_set.contains(&mn.masternode.address) {
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

    /// Decode the active-masternodes bitmap.
    ///
    /// Returns the masternodes whose bit is set to 1, in slot_id order.
    /// The order MUST match `create_active_bitmap_from_voters` exactly.
    pub async fn get_active_from_bitmap(&self, bitmap: &[u8]) -> Vec<MasternodeInfo> {
        let masternodes = self.masternodes.read().await;

        // Same sort order as create_active_bitmap_from_voters.
        // Valid slot_id nodes first; unregistered (u32::MAX) ordered by address.
        let mut sorted_mns: Vec<MasternodeInfo> = masternodes.values().cloned().collect();
        sorted_mns.sort_by(|a, b| {
            a.masternode
                .slot_id
                .cmp(&b.masternode.slot_id)
                .then_with(|| a.masternode.address.cmp(&b.masternode.address))
        });

        let mut active = Vec::new();
        for (i, mn) in sorted_mns.iter().enumerate() {
            let byte_index = i / 8;
            let bit_index = 7 - (i % 8);
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

    /// Mark a masternode's registration source (e.g., after on-chain collateral verification).
    pub async fn set_registration_source(
        &self,
        address: &str,
        source: RegistrationSource,
    ) -> Result<(), RegistryError> {
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(address) {
            info.registration_source = source;
            self.store_masternode(address, info)?;
        }
        Ok(())
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

    /// Get the local wallet (reward) address — persists even if masternode is deregistered
    pub async fn get_local_wallet_address(&self) -> Option<String> {
        self.local_wallet_address.read().await.clone()
    }

    pub async fn set_local_masternode(&self, address: String) {
        *self.local_masternode_address.write().await = Some(address.clone());
        // Cache the node's own spendable address (wallet_address, not reward_address).
        // reward_address may be an external GUI wallet whose key is not on this server.
        if let Some(info) = self.masternodes.read().await.get(&address) {
            *self.local_wallet_address.write().await = Some(info.masternode.wallet_address.clone());
        }
    }

    /// Get the local masternode's certificate (for announcements)
    pub async fn get_local_certificate(&self) -> [u8; 64] {
        *self.local_certificate.read().await
    }

    /// Set the local masternode's certificate (loaded from masternode.conf)
    pub async fn set_local_certificate(&self, certificate: [u8; 64]) {
        *self.local_certificate.write().await = certificate;
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
        self.count_active().await
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
    pub async fn list_by_tier(&self, tier: MasternodeTier) -> Vec<MasternodeInfo> {
        self.masternodes
            .read()
            .await
            .values()
            .filter(|info| {
                info.is_active
                    && std::mem::discriminant(&info.masternode.tier)
                        == std::mem::discriminant(&tier)
            })
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    pub async fn count(&self) -> usize {
        self.masternodes.read().await.len()
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
            "Collateral validation passed for outpoint {} (tier: {:?}, amount: {})",
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
                // Verify UTXO exists first
                if utxo_manager.get_utxo(collateral_outpoint).await.is_err() {
                    tracing::warn!(
                        "⚠️ Masternode {} collateral {}:{} UTXO no longer exists",
                        masternode_address,
                        hex::encode(collateral_outpoint.txid),
                        collateral_outpoint.vout
                    );
                    return false;
                }

                // Verify UTXO is locked as collateral
                if !utxo_manager.is_collateral_locked(collateral_outpoint) {
                    // UTXO exists but isn't locked — this can happen during block
                    // processing when a recollateralization TX creates the new UTXO
                    // but it hasn't been formally locked yet. Auto-lock it.
                    let lock_height = self
                        .current_height
                        .load(std::sync::atomic::Ordering::Relaxed);
                    match utxo_manager.lock_collateral(
                        collateral_outpoint.clone(),
                        masternode_address.to_string(),
                        lock_height,
                        info.masternode.tier.collateral(),
                    ) {
                        Ok(()) => {
                            tracing::info!(
                                "🔒 Auto-locked collateral {}:{} for masternode {}",
                                hex::encode(collateral_outpoint.txid),
                                collateral_outpoint.vout,
                                masternode_address
                            );
                            return true;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "⚠️ Masternode {} collateral {}:{} exists but could not be locked: {:?}",
                                masternode_address,
                                hex::encode(collateral_outpoint.txid),
                                collateral_outpoint.vout,
                                e
                            );
                            return false;
                        }
                    }
                }

                return true;
            }

            // Legacy masternode without locked collateral - always valid
            true
        } else {
            false
        }
    }

    /// Automatically deregister masternodes whose collateral has been spent.
    /// Uses a 3-block grace period before deregistering to avoid split-brain
    /// caused by transient UTXO-set divergence at block boundaries.
    /// Should be called periodically (e.g., after each block).
    pub async fn cleanup_invalid_collaterals(
        &self,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> usize {
        const MISS_THRESHOLD: u32 = 3; // consecutive misses required before deregistration

        let mut to_deregister = Vec::new();

        // Never auto-deregister the local masternode — operator must disable explicitly
        let local_addr = self.local_masternode_address.read().await.clone();

        // Check all masternodes
        {
            let masternodes = self.masternodes.read().await;
            for (address, info) in masternodes.iter() {
                // Skip the local masternode
                if let Some(ref local) = local_addr {
                    if address == local {
                        continue;
                    }
                }
                // Only check masternodes with locked collateral
                if info.masternode.collateral_outpoint.is_none() {
                    continue;
                }
                if self.check_collateral_validity(address, utxo_manager).await {
                    // Collateral is fine — reset any pending miss count
                    self.collateral_miss_counts.remove(address);
                } else {
                    // Collateral missing — increment miss counter
                    let mut entry = self
                        .collateral_miss_counts
                        .entry(address.clone())
                        .or_insert(0);
                    *entry += 1;
                    let misses = *entry;
                    if misses >= MISS_THRESHOLD {
                        to_deregister.push(address.clone());
                    } else {
                        tracing::warn!(
                            "⚠️ Masternode {} collateral missing ({}/{} consecutive misses — deferring deregistration)",
                            address, misses, MISS_THRESHOLD
                        );
                    }
                }
            }
        }

        // Deregister masternodes that have exceeded the grace period
        let count = to_deregister.len();
        for address in to_deregister {
            self.collateral_miss_counts.remove(&address);
            tracing::warn!(
                "🗑️ Auto-deregistering masternode {} due to invalid collateral ({} consecutive misses)",
                address, MISS_THRESHOLD
            );
            match self.unregister(&address).await {
                Ok(Some(info)) => {
                    if let Some(outpoint) = &info.masternode.collateral_outpoint {
                        let _ = utxo_manager.unlock_collateral(outpoint);
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!("Failed to deregister masternode {}: {:?}", address, e);
                }
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

    /// Start the periodic reachability prober task.
    ///
    /// Every `REACHABILITY_RECHECK_SECS` seconds this task re-probes any active
    /// masternodes whose last probe is stale.  Nodes that are reachable via outbound
    /// connections stay marked reachable as long as the connection remains open;
    /// inbound-only nodes are re-tested so that nodes that fix their port forwarding
    /// eventually become reward-eligible again.
    ///
    /// `registry_arc` must be the same `Arc` that is shared across the node; pass it
    /// in explicitly because `MasternodeRegistry` is not itself wrapped in an Arc here.
    pub fn start_reachability_prober(
        registry_arc: Arc<Self>,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) {
        tokio::spawn(async move {
            // Wait 5 minutes before the first probe pass (let connections stabilise)
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(REACHABILITY_RECHECK_SECS));
            loop {
                interval.tick().await;
                let candidates = registry_arc.get_nodes_needing_reachability_probe().await;
                if candidates.is_empty() {
                    continue;
                }
                tracing::debug!(
                    "🔍 Reachability prober: checking {} node(s)",
                    candidates.len()
                );
                let network = registry_arc.network();
                for addr in candidates {
                    // Skip nodes that have an outbound connection — they're already reachable.
                    let ip_only = addr.split(':').next().unwrap_or(&addr).to_string();
                    if peer_registry.is_outbound(&ip_only) {
                        // Outbound means we can reach them — no TCP probe needed.
                        registry_arc.set_publicly_reachable(&ip_only, true).await;
                        continue;
                    }
                    let registry_clone = Arc::clone(&registry_arc);
                    let peer_registry_clone = Arc::clone(&peer_registry);
                    tokio::spawn(async move {
                        crate::network::message_handler::probe_masternode_reachability(
                            ip_only,
                            network,
                            registry_clone,
                            peer_registry_clone,
                        )
                        .await;
                    });
                }
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

        // Record our own sightings in our local registry BEFORE broadcasting.
        // Without this, a direct peer's peer_reports for masternodes we can see
        // would never include US as a reporter (we only sent gossip outward),
        // causing the gossip-based is_active check to under-count reporters for
        // nodes we are directly connected to.
        self.process_status_gossip(reporter.clone(), visible.clone(), now)
            .await;

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
    pub fn start_report_cleanup(
        &self,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                registry.cleanup_stale_reports(&peer_registry).await;
            }
        });
        tracing::info!("✓ Gossip cleanup task started (runs every 60s)");
    }

    /// Remove stale peer reports and update is_active status
    async fn cleanup_stale_reports(
        &self,
        peer_registry: &crate::network::peer_connection_registry::PeerConnectionRegistry,
    ) {
        let now = Self::now();

        // Pre-fetch async state BEFORE taking the write lock so we never hold
        // masternodes.write() across an async boundary.
        let connected_peers: std::collections::HashSet<String> = peer_registry
            .get_connected_peers()
            .await
            .into_iter()
            .collect();
        let local_addr = self.local_masternode_address.read().await.clone();
        let local_ip = local_addr
            .as_ref()
            .map(|a| a.split(':').next().unwrap_or(a).to_string());

        let mut masternodes = self.masternodes.write().await;

        let mut status_changes = 0;
        let mut total_active = 0;

        // Calculate dynamic threshold once before the loop
        let total_masternodes = masternodes.len();
        let min_reports = if total_masternodes <= 4 {
            // Very small network: require at least half
            (total_masternodes / 2).max(1)
        } else if total_masternodes <= 12 {
            // Small-to-mid network (testnet range): require 2 reporters.
            // Pyramid leaf nodes connect to 5-6 upward peers; each direct peer
            // self-records its own gossip, so a leaf reachable from 2+ nodes
            // will have ≥ 2 reporters in every node's registry.
            2
        } else {
            // Large network: use standard threshold
            MIN_PEER_REPORTS
        };

        for (addr, info) in masternodes.iter_mut() {
            // Never deactivate the local masternode via gossip — it is always
            // running and doesn't report on itself, so it would always have
            // zero peer reports and be incorrectly deactivated.
            let addr_ip = addr.split(':').next().unwrap_or(addr);
            if local_ip.as_deref() == Some(addr_ip) {
                if info.is_active {
                    total_active += 1;
                }
                continue;
            }

            // Handshake nodes' lifecycle is owned by TCP connect/disconnect events.
            // Their presence in the registry already means they ARE directly connected —
            // never let gossip-based report counts flip them inactive.
            if info.registration_source == RegistrationSource::Handshake {
                info.peer_reports
                    .retain(|_, ts| now.saturating_sub(*ts) < REPORT_EXPIRY_SECS);
                if info.is_active {
                    total_active += 1;
                }
                continue;
            }

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

            // Require sufficient report count AND subnet diversity (if network is large enough)
            let meets_count = report_count >= min_reports;
            let meets_diversity = if total_masternodes > 12 && report_count >= 2 {
                // Require witnesses from at least 2 distinct /16 subnets to prevent
                // targeted DDoS against a node's witnesses on the same subnet.
                // Only enforce for larger networks (>12 nodes) — small networks with
                // co-located infrastructure (shared /16 subnets) would otherwise have
                // nodes stuck inactive despite being fully online and reachable.
                let mut subnets = std::collections::HashSet::new();
                for entry in info.peer_reports.iter() {
                    let peer_addr: &String = entry.key();
                    // Extract /16 prefix from IP address (e.g., "192.168" from "192.168.1.1:24000")
                    if let Some(ip_part) = peer_addr.split(':').next() {
                        let octets: Vec<&str> = ip_part.split('.').collect();
                        if octets.len() >= 2 {
                            subnets.insert(format!("{}.{}", octets[0], octets[1]));
                        }
                    }
                }
                subnets.len() >= 2
            } else {
                true // Small networks exempt from diversity requirement
            };
            // A direct TCP connection is authoritative proof of liveness —
            // gossip counts are a secondary signal for nodes we aren't directly
            // connected to. Never flip is_active to false while we have a live
            // connection, regardless of how many gossip reporters we have.
            let is_directly_connected = connected_peers.contains(addr.as_str());
            info.is_active = is_directly_connected || (meets_count && meets_diversity);

            if was_active != info.is_active {
                status_changes += 1;
                tracing::debug!(
                    "Masternode {} status changed: {} ({} peer reports, {} required, direct={})",
                    addr,
                    if info.is_active { "ACTIVE" } else { "INACTIVE" },
                    report_count,
                    min_reports,
                    is_directly_connected
                );
            }

            if info.is_active {
                total_active += 1;
            }
        }

        // Auto-remove masternodes with no peer reports for extended period
        // Skip during startup grace period — peers haven't connected yet so
        // masternodes loaded from disk still have stale timestamps.
        let uptime = now.saturating_sub(self.started_at);
        let mut to_remove = Vec::new();
        if uptime >= STARTUP_GRACE_PERIOD_SECS {
            for (address, info) in masternodes.iter() {
                // Handshake nodes are removed immediately on TCP disconnect — never auto-remove.
                if info.registration_source == RegistrationSource::Handshake {
                    continue;
                }
                // Never auto-remove the local masternode.
                let addr_ip = address.split(':').next().unwrap_or(address);
                if local_ip.as_deref() == Some(addr_ip) {
                    continue;
                }
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
        }

        // Remove dead masternodes
        for address in &to_remove {
            if let Some(info) = masternodes.remove(address) {
                // Remove from disk
                self.sled_remove_bg(format!("masternode:{}", address).into_bytes());

                // Queue collateral unlock and remove on-chain anchor
                if let Some(outpoint) = &info.masternode.collateral_outpoint {
                    self.pending_collateral_unlocks
                        .lock()
                        .push(outpoint.clone());

                    // Remove the collateral_anchor so the outpoint can be
                    // re-registered by a new masternode without being blocked.
                    let outpoint_str = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
                    let anchor_key = format!("collateral_anchor:{}", outpoint_str);
                    self.sled_remove_bg(anchor_key.into_bytes());
                }

                info!(
                    "🗑️  Removed masternode {} from registry (auto-removed after inactivity)",
                    address
                );
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

        self.rebuild_node_caches(&masternodes);
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

    /// Get blocks-without-pool-reward for all masternodes by scanning masternode_rewards.
    /// Unlike `get_verifiable_reward_tracking` (which only checks block leader), this
    /// checks all reward outputs — covering both leader bonus and pool shares.
    /// Single pass over blocks: O(scan_limit × avg_rewards_per_block).
    pub async fn get_pool_reward_tracking(
        &self,
        blockchain: &crate::blockchain::Blockchain,
    ) -> std::collections::HashMap<String, u64> {
        let current_height = blockchain.get_height();
        let masternodes = self.masternodes.read().await;
        let scan_limit = 1000u64;

        if current_height < 10 {
            return masternodes.keys().map(|a| (a.clone(), 0)).collect();
        }

        // Build wallet_address -> masternode_address mapping
        let wallet_to_mn: std::collections::HashMap<&str, &str> = masternodes
            .values()
            .map(|info| {
                (
                    info.masternode.wallet_address.as_str(),
                    info.masternode.address.as_str(),
                )
            })
            .collect();

        // Scan blocks once, record last reward height for each address
        let mut last_rewarded: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let start_h = current_height.saturating_sub(scan_limit);

        for h in (start_h..=current_height).rev() {
            if let Ok(block) = blockchain.get_block(h) {
                for (wallet, _) in &block.masternode_rewards {
                    if let Some(&mn_addr) = wallet_to_mn.get(wallet.as_str()) {
                        last_rewarded.entry(mn_addr.to_string()).or_insert(h);
                    }
                }
            }
        }

        // Convert to blocks_without_reward
        masternodes
            .keys()
            .map(|addr| {
                let blocks_without = match last_rewarded.get(addr) {
                    Some(&h) => current_height.saturating_sub(h),
                    None => scan_limit,
                };
                (addr.clone(), blocks_without)
            })
            .collect()
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

            if let Ok(data) = bincode::serialize(info) {
                self.sled_insert_bg(
                    format!("masternode:{}", masternode_address).into_bytes(),
                    data,
                );
            }

            tracing::debug!(
                "💰 Masternode {} received reward at height {} (counter reset)",
                masternode_address,
                block_height
            );
        }
    }

    /// Update blocks_without_reward counters for all masternodes in a single lock.
    /// For each masternode: if its effective wallet address appears in `rewarded_wallets`,
    /// reset its counter (record reward); otherwise increment it.
    /// Called from add_block after a block is committed — O(n) in-memory only, no sled I/O
    /// except for the periodic persist (every 10 blocks) which uses the background writer.
    pub async fn update_reward_counters(
        &self,
        block_height: u64,
        rewarded_wallets: &std::collections::HashSet<String>,
    ) {
        let persist_now = block_height % 10 == 0;
        let mut masternodes = self.masternodes.write().await;
        for info in masternodes.values_mut() {
            let effective_wallet = if !info.reward_address.is_empty() {
                info.reward_address.as_str()
            } else {
                info.masternode.wallet_address.as_str()
            };
            if rewarded_wallets.contains(effective_wallet) {
                info.last_reward_height = block_height;
                info.blocks_without_reward = 0;
                // Always persist reward resets so restart recovery stays accurate
                if let Ok(data) = bincode::serialize(info) {
                    self.sled_insert_bg(
                        format!("masternode:{}", info.masternode.address).into_bytes(),
                        data,
                    );
                }
            } else {
                info.blocks_without_reward += 1;
                if persist_now {
                    if let Ok(data) = bincode::serialize(info) {
                        self.sled_insert_bg(
                            format!("masternode:{}", info.masternode.address).into_bytes(),
                            data,
                        );
                    }
                }
            }
        }
    }

    /// Get blocks_without_reward for all masternodes from the in-memory counter.
    /// O(n) read with no sled I/O. Use this instead of get_pool_reward_tracking /
    /// get_verifiable_reward_tracking to avoid blocking the async runtime.
    /// Returns a map of masternode_address → blocks_without_reward.
    pub async fn get_reward_tracking_from_memory(
        &self,
    ) -> std::collections::HashMap<String, u64> {
        let masternodes = self.masternodes.read().await;
        masternodes
            .iter()
            .map(|(addr, info)| (addr.clone(), info.blocks_without_reward))
            .collect()
    }

    /// Reconstruct blocks_without_reward counters from persisted last_reward_height.
    /// Called once at startup after the blockchain height is known.
    /// Avoids a full 1000-block scan by computing: current_height - last_reward_height.
    pub async fn reconstruct_reward_counters(&self, current_height: u64) {
        let mut masternodes = self.masternodes.write().await;
        for info in masternodes.values_mut() {
            if info.last_reward_height == 0 {
                // Never rewarded within our history — treat as maximum wait
                info.blocks_without_reward = current_height.min(1000);
            } else {
                info.blocks_without_reward =
                    current_height.saturating_sub(info.last_reward_height);
            }
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
                if let Ok(data) = bincode::serialize(info) {
                    self.sled_insert_bg(
                        format!("masternode:{}", address).into_bytes(),
                        data,
                    );
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

    // ========================================================================
    // On-chain masternode registration validation (Phase 1B)
    // ========================================================================

    /// Validate a MasternodeReg special transaction.
    ///
    /// Checks:
    /// 1. Collateral outpoint exists as an unspent UTXO
    /// 2. Collateral amount meets the tier requirement
    /// 3. Owner pubkey matches the collateral UTXO's address
    /// 4. Signature is valid over the registration fields
    /// 5. Collateral is not already used by another active masternode
    #[allow(clippy::too_many_arguments)]
    pub async fn validate_masternode_reg(
        &self,
        collateral_outpoint_str: &str,
        masternode_ip: &str,
        masternode_port: u16,
        payout_address: &str,
        owner_pubkey_hex: &str,
        signature_hex: &str,
        operator_pubkey_hex: Option<&str>,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> Result<(OutPoint, MasternodeTier, Option<String>), RegistryError> {
        // Parse collateral outpoint "txid_hex:vout"
        let outpoint = Self::parse_outpoint(collateral_outpoint_str)?;

        // 1. Verify collateral UTXO exists and is unspent
        let utxo = utxo_manager
            .get_utxo(&outpoint)
            .await
            .map_err(|_| RegistryError::CollateralNotFound)?;

        // 2. Determine tier from collateral value
        let tier = MasternodeTier::from_collateral_value(utxo.value)
            .ok_or(RegistryError::InvalidCollateral)?;

        // 3. Verify owner pubkey
        let owner_pubkey = Self::parse_pubkey(owner_pubkey_hex)?;
        let expected_address =
            crate::address::Address::from_public_key(owner_pubkey.as_bytes(), self.network)
                .as_string();
        if utxo.address != expected_address {
            return Err(RegistryError::OwnerMismatch);
        }

        // 3b. Enforce payout_address == utxo.address.
        // Rewards must go to the collateral owner — no redirection allowed.
        // This is a mempool/relay rule (not a block-validity rule) so it does not
        // break consensus with existing nodes: blocks produced before this rule are
        // still accepted, but new registrations that try to redirect rewards will not
        // propagate through upgraded nodes and will not be mined.
        if payout_address != expected_address {
            return Err(RegistryError::OwnerMismatch);
        }

        // 4. Verify signature over registration fields
        let message = Self::reg_signing_message(
            collateral_outpoint_str,
            masternode_ip,
            masternode_port,
            payout_address,
        );
        Self::verify_signature(&owner_pubkey, &message, signature_hex)?;

        // 5. Check collateral not already used by another active masternode.
        //    Exception: gossip-only entries (Handshake source) may be evicted by a
        //    valid on-chain registration — the on-chain tx carries a cryptographic
        //    signature over the collateral that proves ownership.  A gossip squatter
        //    can never produce this signature, so a valid MasternodeReg is definitive
        //    proof of ownership and wins over any gossip claim.
        let nodes = self.masternodes.read().await;
        for info in nodes.values() {
            if let Some(ref existing_outpoint) = info.masternode.collateral_outpoint {
                if existing_outpoint.txid == outpoint.txid
                    && existing_outpoint.vout == outpoint.vout
                {
                    // Allow on-chain registration to evict gossip-only squatters.
                    if matches!(info.registration_source, RegistrationSource::Handshake) {
                        tracing::warn!(
                            "🛡️ On-chain MasternodeReg overrides gossip entry for collateral \
                             {}:{} (was held by gossip-only node {})",
                            hex::encode(outpoint.txid),
                            outpoint.vout,
                            info.masternode.address
                        );
                        // Eviction happens in apply_masternode_reg after validation passes.
                    } else {
                        return Err(RegistryError::DuplicateCollateral);
                    }
                }
            }
        }

        // 6. Parse and validate operator_pubkey (if provided).
        //    This is the masternode node's hot key — separate from the owner/wallet key.
        //    We only check it is a valid Ed25519 public key; it does not need to match
        //    the UTXO address (that is the owner key's job).
        let validated_operator_pubkey: Option<String> = if let Some(op_hex) = operator_pubkey_hex {
            Self::parse_pubkey(op_hex).map_err(|_| RegistryError::InvalidSignature)?;
            Some(op_hex.to_string())
        } else {
            None
        };

        Ok((outpoint, tier, validated_operator_pubkey))
    }

    /// Validate a MasternodePayoutUpdate special transaction.
    ///
    /// Checks:
    /// 1. Masternode exists and is registered
    /// 2. Owner pubkey matches the original registration
    /// 3. Signature is valid over (masternode_id, new_payout_address)
    pub async fn validate_masternode_update(
        &self,
        masternode_id: &str,
        new_payout_address: &str,
        owner_pubkey_hex: &str,
        signature_hex: &str,
    ) -> Result<(), RegistryError> {
        // 1. Verify masternode exists
        let nodes = self.masternodes.read().await;
        let info = nodes.get(masternode_id).ok_or(RegistryError::NotFound)?;

        // 2. Verify owner pubkey matches original registration
        let owner_pubkey = Self::parse_pubkey(owner_pubkey_hex)?;
        if info.masternode.public_key != owner_pubkey {
            return Err(RegistryError::OwnerMismatch);
        }

        // 3. Verify signature
        let message = Self::update_signing_message(masternode_id, new_payout_address);
        Self::verify_signature(&owner_pubkey, &message, signature_hex)?;

        Ok(())
    }

    /// Apply a validated MasternodeReg: register the masternode with on-chain data.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_masternode_reg(
        &self,
        outpoint: OutPoint,
        masternode_ip: &str,
        _masternode_port: u16,
        payout_address: &str,
        owner_pubkey_hex: &str,
        operator_pubkey_hex: Option<&str>,
        tier: MasternodeTier,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> Result<(), RegistryError> {
        let owner_pubkey = Self::parse_pubkey(owner_pubkey_hex)?;
        // Use IP-only as the registry key — consistent with startup registration
        // (main.rs strips port via split(':').next()) and P2P announcement handling
        // (server.rs does peer.addr.split(':').next()).  Using "IP:port" would create
        // a duplicate entry and prevent RegistrationSource::OnChain from ever being set.
        let address = masternode_ip.to_string();

        let masternode = Masternode::new_with_collateral(
            address.clone(),
            payout_address.to_string(),
            tier.collateral(),
            outpoint.clone(),
            owner_pubkey,
            tier,
            Self::now(),
        );

        // Lock the collateral UTXO
        let height = self
            .current_height
            .load(std::sync::atomic::Ordering::Relaxed);
        let _ = utxo_manager.lock_collateral(
            outpoint.clone(),
            address.clone(),
            height,
            tier.collateral(),
        );

        // Evict any gossip-only squatter holding this collateral before registering.
        // validate_masternode_reg already verified the signature proves ownership, so
        // any gossip-only entry for this collateral is illegitimate.  The on-chain
        // MasternodeReg is definitive proof — evict the squatter and record their IP
        // in the incompatible-peers map so they are excluded from sync decisions.
        {
            let mut nodes = self.masternodes.write().await;
            let squatter_addr = nodes
                .iter()
                .filter(|(addr, info)| {
                    *addr != &address
                        && matches!(info.registration_source, RegistrationSource::Handshake)
                        && info
                            .masternode
                            .collateral_outpoint
                            .as_ref()
                            .map(|op| op.txid == outpoint.txid && op.vout == outpoint.vout)
                            .unwrap_or(false)
                })
                .map(|(addr, _)| addr.clone())
                .next();

            if let Some(squatter) = squatter_addr {
                tracing::error!("🚨 ════════════════════════════════════════════════════════════");
                tracing::error!("🚨 COLLATERAL SQUATTER EVICTED");
                tracing::error!("🚨 IP: {}", squatter);
                tracing::error!(
                    "🚨 Collateral: {}:{}",
                    hex::encode(outpoint.txid),
                    outpoint.vout
                );
                tracing::error!(
                    "🚨 {} held this collateral via gossip without ownership proof.",
                    squatter
                );
                tracing::error!(
                    "🚨 Legitimate owner {} proved ownership via signed on-chain tx.",
                    address
                );
                tracing::error!("🚨 ════════════════════════════════════════════════════════════");
                nodes.remove(&squatter);
            }
            self.rebuild_node_caches(&nodes);
        }

        // Register in the registry (insert or update existing entry)
        self.register(masternode, payout_address.to_string())
            .await?;

        // Mark as on-chain registration and store the operator pubkey (two-key model).
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(&address) {
            info.registration_source = RegistrationSource::OnChain(height);
            // Store the operator key if one was provided (Dash-style owner/operator split).
            // Fall back to the owner key for legacy single-key registrations.
            info.operator_pubkey = operator_pubkey_hex
                .map(|s| s.to_string())
                .or_else(|| Some(owner_pubkey_hex.to_string()));
            self.store_masternode(&address, info)?;
        }
        drop(nodes);

        // Write (or overwrite) the canonical anchor for this collateral outpoint.
        // On-chain registrations are authoritative: they include a signature proving
        // the registrant controls the collateral private key.  This anchor ensures
        // that future gossip claims from a different IP are rejected.
        let outpoint_key = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
        let anchor_db_key = format!("collateral_anchor:{}", outpoint_key);
        if let Err(e) = self.db.insert(anchor_db_key.as_bytes(), address.as_bytes()) {
            tracing::warn!(
                "⚠️ Failed to write on-chain collateral anchor for {}: {}",
                outpoint_key,
                e
            );
        } else {
            tracing::info!(
                "📌 On-chain collateral anchor set: {} → {} (height {})",
                outpoint_key,
                address,
                height
            );
        }

        Ok(())
    }

    /// Apply a validated MasternodePayoutUpdate: change the payout address.
    pub async fn apply_masternode_update(
        &self,
        masternode_id: &str,
        new_payout_address: &str,
    ) -> Result<(), RegistryError> {
        let mut nodes = self.masternodes.write().await;
        let info = nodes
            .get_mut(masternode_id)
            .ok_or(RegistryError::NotFound)?;

        info.reward_address = new_payout_address.to_string();
        info.masternode.wallet_address = new_payout_address.to_string();

        // Persist to disk
        self.store_masternode(masternode_id, info)?;

        info!(
            "📝 Masternode {} payout address updated to {}",
            masternode_id, new_payout_address
        );

        Ok(())
    }

    // ========================================================================
    // On-chain collateral unlock (CollateralUnlock special transaction)
    // ========================================================================

    /// Return the collateral outpoint string ("txid_hex:vout") that is currently
    /// anchored on-chain for the given masternode IP, or None if not found.
    pub fn get_on_chain_collateral_outpoint_for_ip(&self, ip: &str) -> Option<String> {
        for item in self.db.scan_prefix(b"collateral_anchor:").flatten() {
            if let Ok(stored_ip) = String::from_utf8(item.1.to_vec()) {
                if stored_ip == ip {
                    if let Ok(key_str) = String::from_utf8(item.0.to_vec()) {
                        return key_str
                            .strip_prefix("collateral_anchor:")
                            .map(|s| s.to_string());
                    }
                }
            }
        }
        None
    }

    /// Returns the permanent slot ID for a node address, or `None` if not yet assigned.
    pub async fn get_slot_id_for_address(&self, address: &str) -> Option<u32> {
        let nodes = self.masternodes.read().await;
        nodes.get(address).and_then(|info| {
            if info.masternode.slot_id == u32::MAX {
                None
            } else {
                Some(info.masternode.slot_id)
            }
        })
    }

    /// Apply a `MasternodeRegistration` special transaction confirmed in a block.
    ///
    /// Assigns the next available slot ID and stores the on-chain record.
    /// Returns the assigned slot ID on success.
    pub async fn apply_masternode_registration(
        &self,
        node_address: &str,
        wallet_address: &str,
        reward_address: &str,
        collateral_outpoint: &str,
        pubkey: &str,
        signature: &str,
        registration_height: u64,
        registration_txid: &str,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> Result<u32, String> {
        use ed25519_dalek::Verifier;

        // Idempotency guard: if this node is already registered with the same txid
        // (applied earlier at SpentFinalized by apply_finality_result), skip re-application.
        // If registration_height > 0 (i.e. we have the real block height now), update it.
        let key = format!("mnreg:{}", node_address);
        if let Ok(Some(existing_bytes)) = self.db.get(key.as_bytes()) {
            if let Ok(mut existing) = bincode::deserialize::<crate::types::MasternodeOnchainInfo>(&existing_bytes) {
                if existing.registration_txid == registration_txid {
                    // Already applied. If we now have the real block height, update it.
                    if registration_height > 0 && existing.registration_height == 0 {
                        existing.registration_height = registration_height;
                        if let Ok(bytes) = bincode::serialize(&existing) {
                            let _ = self.db.insert(key.as_bytes(), bytes);
                        }
                        // Also update in-memory registration_height
                        if let Some(info) = self.masternodes.write().await.get_mut(node_address) {
                            info.registration_height = registration_height;
                            if let RegistrationSource::OnChain(_) = info.registration_source {
                                info.registration_source = RegistrationSource::OnChain(registration_height);
                            }
                        }
                        tracing::debug!(
                            "↪ MasternodeRegistration height updated: {} height={}",
                            node_address, registration_height
                        );
                    }
                    return Ok(existing.slot_id);
                }
            }
        }

        // Verify signature
        let pubkey_bytes = hex::decode(pubkey).map_err(|e| format!("invalid pubkey hex: {}", e))?;
        let pubkey_arr: [u8; 32] = pubkey_bytes.try_into().map_err(|_| "pubkey must be 32 bytes")?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&pubkey_arr)
            .map_err(|e| format!("invalid Ed25519 pubkey: {}", e))?;
        let sig_bytes = hex::decode(signature).map_err(|e| format!("invalid sig hex: {}", e))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| "signature must be 64 bytes")?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
        let collateral_field = if collateral_outpoint.is_empty() { "none" } else { collateral_outpoint };
        let msg = format!("MNREG:{}:{}:{}:{}", node_address, wallet_address, pubkey, collateral_field);
        verifying_key.verify(msg.as_bytes(), &sig)
            .map_err(|_| "MasternodeRegistration signature verification failed")?;

        // Assign next slot ID (atomically from sled counter)
        let slot_id = self.assign_next_slot_id()?;

        // Determine tier and lock collateral for paid-tier nodes
        let (tier, collateral_amount, outpoint_opt) = if collateral_outpoint.is_empty() {
            (crate::types::MasternodeTier::Free, 0u64, None)
        } else {
            let parts: Vec<&str> = collateral_outpoint.split(':').collect();
            if parts.len() != 2 {
                return Err(format!("invalid collateral_outpoint format: {}", collateral_outpoint));
            }
            let txid_bytes = hex::decode(parts[0]).map_err(|e| format!("txid hex: {}", e))?;
            let txid_arr: [u8; 32] = txid_bytes.try_into().map_err(|_| "txid must be 32 bytes")?;
            let vout: u32 = parts[1].parse().map_err(|e| format!("vout: {}", e))?;
            let outpoint = crate::types::OutPoint { txid: txid_arr, vout };
            let utxo = utxo_manager.get_utxo(&outpoint).await
                .map_err(|e| format!("UTXO lookup failed: {}", e))?;
            let tier = MasternodeTier::from_collateral_value(utxo.value)
                .unwrap_or(crate::types::MasternodeTier::Free);
            (tier, utxo.value, Some(outpoint))
        };

        // Build and register the masternode
        let public_key = verifying_key;
        let now = chrono::Utc::now().timestamp() as u64;
        let mut mn = crate::types::Masternode::new_legacy(
            node_address.to_string(),
            wallet_address.to_string(),
            collateral_amount,
            public_key,
            tier,
            now,
        );
        mn.reward_address = reward_address.to_string();
        mn.collateral_outpoint = outpoint_opt;
        mn.slot_id = slot_id;

        // Store the on-chain record
        let record = crate::types::MasternodeOnchainInfo {
            node_address: node_address.to_string(),
            wallet_address: wallet_address.to_string(),
            reward_address: reward_address.to_string(),
            collateral_outpoint: collateral_outpoint.to_string(),
            pubkey: pubkey.to_string(),
            slot_id,
            registration_height,
            registration_txid: registration_txid.to_string(),
        };
        let key = format!("mnreg:{}", node_address);
        let bytes = bincode::serialize(&record).map_err(|e| format!("serialize: {}", e))?;
        self.db.insert(key.as_bytes(), bytes).map_err(|e| format!("db insert: {}", e))?;

        // Register in the in-memory registry
        let mn_info = MasternodeInfo {
            masternode: mn,
            reward_address: reward_address.to_string(),
            registration_source: RegistrationSource::OnChain(registration_height),
            registration_height,
            uptime_start: now,
            total_uptime: 0,
            is_active: false,
            daemon_started_at: 0,
            last_reward_height: 0,
            blocks_without_reward: 0,
            peer_reports: Arc::new(Default::default()),
            operator_pubkey: None,
            is_publicly_reachable: false,
            reachability_checked_at: 0,
            first_seen_at: now,
            last_seen_at: 0,
        };
        self.masternodes.write().await.insert(node_address.to_string(), mn_info);

        tracing::info!(
            "✅ Masternode registered on-chain: {} tier={:?} slot={} height={}",
            node_address, tier, slot_id, registration_height
        );

        Ok(slot_id)
    }

    /// Apply a `MasternodeDeregistration` special transaction confirmed in a block.
    pub async fn apply_masternode_deregistration(
        &self,
        node_address: &str,
        slot_id: u32,
        pubkey: &str,
        signature: &str,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> Result<(), String> {
        use ed25519_dalek::Verifier;

        // Verify signature
        let pubkey_bytes = hex::decode(pubkey).map_err(|e| format!("invalid pubkey hex: {}", e))?;
        let pubkey_arr: [u8; 32] = pubkey_bytes.try_into().map_err(|_| "pubkey must be 32 bytes")?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&pubkey_arr)
            .map_err(|e| format!("invalid Ed25519 pubkey: {}", e))?;
        let sig_bytes = hex::decode(signature).map_err(|e| format!("invalid sig hex: {}", e))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| "signature must be 64 bytes")?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
        let msg = format!("MNDEREG:{}:{}", node_address, slot_id);
        verifying_key.verify(msg.as_bytes(), &sig)
            .map_err(|_| "MasternodeDeregistration signature verification failed")?;

        // Idempotency guard: if already deregistered (applied at SpentFinalized), return Ok.
        {
            let nodes = self.masternodes.read().await;
            if let Some(info) = nodes.get(node_address) {
                if info.masternode.slot_id != slot_id {
                    return Err(format!(
                        "slot_id mismatch: registered {}, got {}",
                        info.masternode.slot_id, slot_id
                    ));
                }
                // Unlock collateral if paid tier
                if let Some(ref outpoint) = info.masternode.collateral_outpoint {
                    let _ = utxo_manager.unlock_collateral(outpoint);
                }
            } else {
                // Already removed (applied at SpentFinalized) — idempotent, nothing to do.
                tracing::debug!(
                    "↪ MasternodeDeregistration already applied (idempotent): {} slot={}",
                    node_address, slot_id
                );
                return Ok(());
            }
        }

        // Remove from in-memory registry and sled
        self.masternodes.write().await.remove(node_address);
        let key = format!("mnreg:{}", node_address);
        let _ = self.db.remove(key.as_bytes());

        tracing::info!("✅ Masternode deregistered: {} slot={}", node_address, slot_id);
        Ok(())
    }

    /// Apply a `MasternodePayoutUpdate` special transaction confirmed in a block.
    pub async fn apply_masternode_payout_update(
        &self,
        node_address: &str,
        new_reward_address: &str,
        pubkey: &str,
        signature: &str,
    ) -> Result<(), String> {
        use ed25519_dalek::Verifier;

        let pubkey_bytes = hex::decode(pubkey).map_err(|e| format!("invalid pubkey hex: {}", e))?;
        let pubkey_arr: [u8; 32] = pubkey_bytes.try_into().map_err(|_| "pubkey must be 32 bytes")?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&pubkey_arr)
            .map_err(|e| format!("invalid Ed25519 pubkey: {}", e))?;
        let sig_bytes = hex::decode(signature).map_err(|e| format!("invalid sig hex: {}", e))?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| "signature must be 64 bytes")?;
        let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
        let msg = format!("MNPAYOUT:{}:{}", node_address, new_reward_address);
        verifying_key.verify(msg.as_bytes(), &sig)
            .map_err(|_| "MasternodePayoutUpdate signature verification failed")?;

        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(node_address) {
            info.masternode.reward_address = new_reward_address.to_string();
            info.reward_address = new_reward_address.to_string();
            self.store_masternode(node_address, info).map_err(|e| format!("store: {}", e))?;
        } else {
            // Node may have been deregistered between SpentFinalized and block archival
            // — treat as already-applied, not an error.
            tracing::debug!(
                "↪ MasternodePayoutUpdate for unknown node (idempotent): {}",
                node_address
            );
        }
        Ok(())
    }

    /// Assign the next available slot ID from the persistent counter in sled.
    fn assign_next_slot_id(&self) -> Result<u32, String> {
        let key = b"next_slot_id";
        let current = self.db.get(key)
            .map_err(|e| format!("sled get: {}", e))?
            .and_then(|v| {
                let arr: Option<[u8; 4]> = v.as_ref().try_into().ok();
                arr.map(u32::from_le_bytes)
            })
            .unwrap_or(0u32);
        let next = current.checked_add(1).ok_or_else(|| "slot ID overflow".to_string())?;
        self.db.insert(key, next.to_le_bytes().as_ref()).map_err(|e| format!("sled insert: {}", e))?;
        Ok(current)
    }

    /// Look up the registered operator pubkey (hex) for a given collateral outpoint.
    ///
    /// Returns `Some(hex)` if the collateral has an on-chain registration with an operator key.
    /// Used by gossip/handshake handlers to verify the announcing node is the registered operator.
    pub async fn get_operator_pubkey_for_collateral(&self, outpoint: &OutPoint) -> Option<String> {
        let nodes = self.masternodes.read().await;
        nodes
            .values()
            .find(|info| {
                matches!(info.registration_source, RegistrationSource::OnChain(_))
                    && info
                        .masternode
                        .collateral_outpoint
                        .as_ref()
                        .map(|op| op.txid == outpoint.txid && op.vout == outpoint.vout)
                        .unwrap_or(false)
            })
            .and_then(|info| info.operator_pubkey.clone())
    }

    /// Return the canonical IP address anchored to this collateral outpoint in sled,
    /// if any.  The anchor is written by the first peer to register the outpoint (gossip)
    /// or by an on-chain MasternodeReg tx (on-chain, always wins over gossip).
    pub fn get_collateral_anchor(&self, outpoint: &OutPoint) -> Option<String> {
        let outpoint_key = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
        let anchor_db_key = format!("collateral_anchor:{}", outpoint_key);
        self.db
            .get(anchor_db_key.as_bytes())
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
    }

    /// Return the IP of the masternode currently claiming the given collateral outpoint
    /// in the in-memory `nodes` map, or `None` if no node claims it.
    ///
    /// Used by the message handler to detect registry conflicts when the UTXOManager
    /// lock was lost (e.g., after a restart) but the gossip registry entry survived.
    pub async fn get_registered_ip_for_collateral(&self, outpoint: &OutPoint) -> Option<String> {
        let nodes = self.masternodes.read().await;
        for (addr, info) in nodes.iter() {
            if let Some(ref op) = info.masternode.collateral_outpoint {
                if op.txid == outpoint.txid && op.vout == outpoint.vout {
                    return Some(addr.clone());
                }
            }
        }
        None
    }

    /// Return the reward_address stored for a given masternode IP, or `None` if not found.
    ///
    /// Used by the eviction logic to determine whether a squatter is using the UTXO
    /// owner's address (address-match stalemate) or their own address (safe to evict).
    pub async fn get_reward_address_for_ip(&self, ip: &str) -> Option<String> {
        let nodes = self.masternodes.read().await;
        nodes.get(ip).map(|info| info.reward_address.clone())
    }

    /// Validate a CollateralUnlock special transaction.
    ///
    /// Checks:
    /// 1. Collateral outpoint can be parsed
    /// 2. Owner pubkey is well-formed
    /// 3. Signature is valid over the unlock message
    /// 4. The on-chain anchor for this outpoint exists and points to masternode_address
    pub async fn validate_collateral_unlock(
        &self,
        collateral_outpoint_str: &str,
        masternode_address: &str,
        owner_pubkey_hex: &str,
        signature_hex: &str,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> Result<OutPoint, RegistryError> {
        // 1. Parse outpoint
        let outpoint = Self::parse_outpoint(collateral_outpoint_str)?;

        // 2. Parse owner pubkey
        let owner_pubkey = Self::parse_pubkey(owner_pubkey_hex)?;

        // 3. Verify signature
        let message = Self::unlock_signing_message(collateral_outpoint_str, masternode_address);
        Self::verify_signature(&owner_pubkey, &message, signature_hex)?;

        // 4. Check the on-chain anchor: the outpoint must be registered to this IP
        let anchor_key = format!("collateral_anchor:{}", collateral_outpoint_str);
        match self.db.get(anchor_key.as_bytes()) {
            Ok(Some(bytes)) => {
                let anchored_ip = String::from_utf8(bytes.to_vec()).unwrap_or_default();
                if anchored_ip != masternode_address {
                    return Err(RegistryError::OwnerMismatch);
                }
            }
            Ok(None) => {
                // No anchor: not registered on-chain or already unlocked — idempotent, accept.
            }
            Err(e) => return Err(RegistryError::Storage(e.to_string())),
        }

        // 5. Verify owner_pubkey actually owns the collateral UTXO.
        // Ground truth is the UTXO's address on-chain — not the registry entry, which may have
        // been gossip-filled by an attacker with their own key. This check ensures only the real
        // collateral owner can unlock, even when a squatter has gossip-registered against the same
        // outpoint.
        let utxo = utxo_manager
            .get_utxo(&outpoint)
            .await
            .map_err(|_| RegistryError::CollateralNotFound)?;
        let expected_address =
            crate::address::Address::from_public_key(owner_pubkey.as_bytes(), self.network)
                .as_string();
        if utxo.address != expected_address {
            return Err(RegistryError::OwnerMismatch);
        }

        Ok(outpoint)
    }

    /// Apply a validated CollateralUnlock: remove the on-chain anchor, unlock the UTXO,
    /// and downgrade (or deregister) the masternode to Free tier.
    pub async fn apply_collateral_unlock(
        &self,
        outpoint: OutPoint,
        masternode_address: &str,
        utxo_manager: &crate::utxo_manager::UTXOStateManager,
    ) -> Result<(), RegistryError> {
        let outpoint_str = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
        let anchor_key = format!("collateral_anchor:{}", outpoint_str);

        // Remove the on-chain anchor so gossip can no longer reference this collateral
        if let Err(e) = self.db.remove(anchor_key.as_bytes()) {
            warn!(
                "⚠️ Failed to remove collateral anchor {}: {}",
                outpoint_str, e
            );
        }

        // Unlock the collateral (removes from locked_collaterals map)
        let _ = utxo_manager.unlock_collateral(&outpoint);

        // Downgrade masternode to Free tier (or remove if desired — we keep the entry)
        let mut nodes = self.masternodes.write().await;
        if let Some(info) = nodes.get_mut(masternode_address) {
            info.masternode.collateral_outpoint = None;
            info.masternode.collateral = 0;
            info.masternode.tier = crate::types::MasternodeTier::Free;
            info.registration_source = crate::masternode_registry::RegistrationSource::Handshake;
            self.store_masternode(masternode_address, info)?;
            info!(
                "🔓 CollateralUnlock applied: {} downgraded to Free tier (outpoint {})",
                masternode_address, outpoint_str
            );
        } else {
            info!(
                "🔓 CollateralUnlock applied: anchor removed for {} (masternode not in registry)",
                outpoint_str
            );
        }

        self.rebuild_node_caches(&nodes);
        Ok(())
    }

    // --- Helpers for on-chain registration ---

    /// Parse "txid_hex:vout" into an OutPoint
    fn parse_outpoint(s: &str) -> Result<OutPoint, RegistryError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 2 {
            return Err(RegistryError::Storage(format!(
                "Invalid outpoint format: {}",
                s
            )));
        }
        let txid_bytes =
            hex::decode(parts[0]).map_err(|e| RegistryError::Storage(e.to_string()))?;
        if txid_bytes.len() != 32 {
            return Err(RegistryError::Storage("txid must be 32 bytes".to_string()));
        }
        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);
        let vout: u32 = parts[1]
            .parse()
            .map_err(|e: std::num::ParseIntError| RegistryError::Storage(e.to_string()))?;
        Ok(OutPoint { txid, vout })
    }

    /// Parse hex-encoded Ed25519 public key
    fn parse_pubkey(hex_str: &str) -> Result<ed25519_dalek::VerifyingKey, RegistryError> {
        let bytes = hex::decode(hex_str).map_err(|e| RegistryError::Storage(e.to_string()))?;
        if bytes.len() != 32 {
            return Err(RegistryError::Storage(
                "Public key must be 32 bytes".to_string(),
            ));
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&bytes);
        ed25519_dalek::VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| RegistryError::Storage(format!("Invalid public key: {}", e)))
    }

    /// Construct the canonical message for MasternodeReg signing
    fn reg_signing_message(
        collateral_outpoint: &str,
        masternode_ip: &str,
        masternode_port: u16,
        payout_address: &str,
    ) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let msg = format!(
            "MN_REG:{}:{}:{}:{}",
            collateral_outpoint, masternode_ip, masternode_port, payout_address
        );
        Sha256::digest(msg.as_bytes()).to_vec()
    }

    /// Construct the canonical message for CollateralUnlock signing
    fn unlock_signing_message(collateral_outpoint: &str, masternode_address: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let msg = format!("MN_UNLOCK:{}:{}", collateral_outpoint, masternode_address);
        Sha256::digest(msg.as_bytes()).to_vec()
    }

    /// Construct the canonical message for MasternodePayoutUpdate signing
    fn update_signing_message(masternode_id: &str, new_payout_address: &str) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let msg = format!("MN_UPDATE:{}:{}", masternode_id, new_payout_address);
        Sha256::digest(msg.as_bytes()).to_vec()
    }

    /// Verify an Ed25519 signature over a message
    fn verify_signature(
        pubkey: &ed25519_dalek::VerifyingKey,
        message: &[u8],
        signature_hex: &str,
    ) -> Result<(), RegistryError> {
        use ed25519_dalek::Verifier;
        let sig_bytes =
            hex::decode(signature_hex).map_err(|e| RegistryError::Storage(e.to_string()))?;
        if sig_bytes.len() != 64 {
            return Err(RegistryError::InvalidSignature);
        }
        let signature = ed25519_dalek::Signature::from_bytes(
            sig_bytes
                .as_slice()
                .try_into()
                .map_err(|_| RegistryError::InvalidSignature)?,
        );
        pubkey
            .verify(message, &signature)
            .map_err(|_| RegistryError::InvalidSignature)
    }
}

impl Clone for MasternodeRegistry {
    fn clone(&self) -> Self {
        Self {
            masternodes: self.masternodes.clone(),
            local_masternode_address: self.local_masternode_address.clone(),
            local_wallet_address: self.local_wallet_address.clone(),
            local_certificate: self.local_certificate.clone(),
            db: self.db.clone(),
            sled_write_tx: self.sled_write_tx.clone(),
            network: self.network,
            block_period_start: self.block_period_start.clone(),
            peer_manager: self.peer_manager.clone(),
            broadcast_tx: self.broadcast_tx.clone(),
            started_at: self.started_at,
            current_height: self.current_height.clone(),
            pending_collateral_unlocks: self.pending_collateral_unlocks.clone(),
            pending_collateral_locks: self.pending_collateral_locks.clone(),
            collateral_miss_counts: self.collateral_miss_counts.clone(),
            collateral_migration_times: self.collateral_migration_times.clone(),
            collateral_migration_from: self.collateral_migration_from.clone(),
            post_eviction_lockout: self.post_eviction_lockout.clone(),
            collateral_migration_counts: self.collateral_migration_counts.clone(),
            free_tier_subnet_counts: self.free_tier_subnet_counts.clone(),
            free_tier_reconnect_cooldown: self.free_tier_reconnect_cooldown.clone(),
            priority_reconnect_notify: self.priority_reconnect_notify.clone(),
            utxo_manager: self.utxo_manager.clone(),
            cached_active: parking_lot::RwLock::new(self.cached_active.read().clone()),
            cached_all: parking_lot::RwLock::new(self.cached_all.read().clone()),
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
        let tier_weight = mn.tier.sampling_weight(); // §5.2/§9.2: use sampling weight for VRF sortition
        let blocks_without = blocks_without_reward.get(&mn.address).copied().unwrap_or(0);

        // Fairness bonus: +1 per 10 blocks without reward, uncapped (§5.2.1)
        let fairness_bonus = blocks_without / 10;
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
            masternode_key: None,
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

        // Run cleanup — miss threshold is 3 consecutive misses before deregistration
        registry.cleanup_invalid_collaterals(&utxo_manager).await;
        assert_eq!(
            registry.count().await,
            1,
            "Should still be registered after 1 miss"
        );
        registry.cleanup_invalid_collaterals(&utxo_manager).await;
        assert_eq!(
            registry.count().await,
            1,
            "Should still be registered after 2 misses"
        );
        let cleanup_count = registry.cleanup_invalid_collaterals(&utxo_manager).await;

        // Masternode should be removed after 3 consecutive misses
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
