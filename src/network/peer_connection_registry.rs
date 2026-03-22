//! Peer Connection Registry
//! Manages active peer connections and message routing.
//! Note: Some methods are scaffolding for future peer management features.

#![allow(dead_code)]

use crate::consensus::ConsensusEngine;
use crate::network::message::NetworkMessage;
use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, info, warn};

/// Channel-based writer: sends pre-serialized frame bytes to a dedicated I/O task.
/// This avoids `tokio::io::split()` on TLS streams, which causes frame corruption
/// due to shared internal mutex and waker issues.
pub type PeerWriterTx = mpsc::UnboundedSender<Vec<u8>>;
type ResponseSender = oneshot::Sender<NetworkMessage>;
type ChainTip = (u64, [u8; 32]); // (height, block_hash)

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    Inbound,
    Outbound,
}

#[derive(Clone)]
struct ConnectionState {
    direction: ConnectionDirection,
    #[allow(dead_code)]
    connected_at: Instant,
}

/// State for tracking reconnection backoff
#[derive(Clone)]
struct ReconnectionState {
    next_attempt: Instant,
    #[allow(dead_code)]
    attempt_count: u64,
}

/// Registry of active peer connections with ability to send targeted messages
/// Combines both connection tracking and message routing
pub struct PeerConnectionRegistry {
    // Connection state tracking (lock-free with DashMap)
    connections: DashMap<String, ConnectionState>,
    // Track reconnection backoff
    reconnecting: DashMap<String, ReconnectionState>,
    // Local IP - set once, read many (lock-free with ArcSwapOption)
    local_ip: ArcSwapOption<String>,
    // Metrics (atomic, no locks)
    inbound_count: AtomicUsize,
    outbound_count: AtomicUsize,
    // Map of peer IP to their write channel (sends pre-serialized frame bytes to I/O task)
    peer_writers: Arc<RwLock<HashMap<String, PeerWriterTx>>>,
    // Map of peer IP to their reported blockchain height
    peer_heights: Arc<RwLock<HashMap<String, u64>>>,
    // Map of peer IP to their latest ping RTT in seconds
    peer_ping_times: Arc<RwLock<HashMap<String, f64>>>,
    // Map of peer IP to their chain tip (height + hash)
    peer_chain_tips: Arc<RwLock<HashMap<String, ChainTip>>>,
    // Pending responses for request/response pattern
    pending_responses: Arc<RwLock<HashMap<String, Vec<ResponseSender>>>>,
    // TimeLock consensus resources (shared from server)
    timelock_consensus: Arc<RwLock<Option<Arc<ConsensusEngine>>>>,
    timelock_block_cache: Arc<RwLock<Option<Arc<crate::network::block_cache::BlockCache>>>>,
    timelock_broadcast: Arc<RwLock<Option<broadcast::Sender<NetworkMessage>>>>,
    // WebSocket transaction event sender for real-time wallet notifications
    ws_tx_event_sender:
        Arc<RwLock<Option<broadcast::Sender<crate::rpc::websocket::TransactionEvent>>>>,
    // Blacklist reference for checking whitelist status
    blacklist: Arc<RwLock<Option<Arc<RwLock<crate::network::blacklist::IPBlacklist>>>>>,
    // Discovered peer candidates from peer exchange
    discovered_peers: Arc<RwLock<HashSet<String>>>,
    // Peers on incompatible chains (different hash calculation)
    // Maps peer IP -> (marked_at_timestamp, reason, is_permanent)
    // Permanent incompatibility (genesis mismatch) is never rechecked
    // Temporary incompatibility (hash mismatch) is rechecked after timeout
    incompatible_peers: Arc<RwLock<HashMap<String, IncompatiblePeerInfo>>>,
    // Persistent fork error counter per peer (tracks errors across multiple block requests)
    // Maps peer IP -> error count (resets on successful block add)
    fork_error_counts: DashMap<String, u32>,
    // Notified when any peer's chain tip is updated (for event-driven consensus checks)
    chain_tip_updated: Arc<tokio::sync::Notify>,
    // Cached result of get_compatible_peers() to avoid repeated lock acquisitions
    compatible_peers_cache: Arc<RwLock<(Vec<String>, std::time::Instant)>>,
    // Reported connection counts from peer exchange — used for load-aware routing
    peer_load: DashMap<String, u16>,
}

fn extract_ip(addr: &str) -> &str {
    addr.split(':').next().unwrap_or(addr)
}

/// Information about an incompatible peer
/// (marked_timestamp, incompatibility_reason, is_permanent)
type IncompatiblePeerInfo = (std::time::Instant, String, bool);
/// Type alias for shared writer channel that can be cloned and registered
pub type SharedPeerWriter = PeerWriterTx;

impl PeerConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            reconnecting: DashMap::new(),
            local_ip: ArcSwapOption::empty(),
            inbound_count: AtomicUsize::new(0),
            outbound_count: AtomicUsize::new(0),
            peer_writers: Arc::new(RwLock::new(HashMap::new())),
            peer_heights: Arc::new(RwLock::new(HashMap::new())),
            peer_ping_times: Arc::new(RwLock::new(HashMap::new())),
            peer_chain_tips: Arc::new(RwLock::new(HashMap::new())),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            timelock_consensus: Arc::new(RwLock::new(None)),
            timelock_block_cache: Arc::new(RwLock::new(None)),
            timelock_broadcast: Arc::new(RwLock::new(None)),
            ws_tx_event_sender: Arc::new(RwLock::new(None)),
            blacklist: Arc::new(RwLock::new(None)),
            discovered_peers: Arc::new(RwLock::new(HashSet::new())),
            incompatible_peers: Arc::new(RwLock::new(HashMap::new())),
            fork_error_counts: DashMap::new(),
            chain_tip_updated: Arc::new(tokio::sync::Notify::new()),
            compatible_peers_cache: Arc::new(RwLock::new((Vec::new(), std::time::Instant::now()))),
            peer_load: DashMap::new(),
        }
    }

    /// Set blacklist reference (called once after server initialization)
    pub async fn set_blacklist(
        &self,
        blacklist: Arc<RwLock<crate::network::blacklist::IPBlacklist>>,
    ) {
        *self.blacklist.write().await = Some(blacklist);
    }

    /// Check if a peer IP is whitelisted (trusted masternode from time-coin.io)
    pub async fn is_whitelisted(&self, peer_ip: &str) -> bool {
        let ip_only = extract_ip(peer_ip);
        if let Ok(ip_addr) = ip_only.parse::<IpAddr>() {
            if let Some(blacklist) = self.blacklist.read().await.as_ref() {
                return blacklist.read().await.is_whitelisted(ip_addr);
            }
        }
        false
    }

    /// Duration after which incompatible peers are re-checked (5 minutes)
    /// Note: Genesis mismatch is PERMANENT and never rechecked
    const INCOMPATIBLE_RECHECK_SECS: u64 = 300;
    /// TTL for the compatible peers cache (seconds)
    const COMPATIBLE_PEERS_CACHE_TTL: u64 = 10;

    /// Mark a peer as incompatible (different chain/hash calculation)
    /// If `permanent` is true (genesis mismatch), peer is never rechecked
    /// If `permanent` is false, peer is rechecked after INCOMPATIBLE_RECHECK_SECS
    pub async fn mark_incompatible(&self, peer_ip: &str, reason: &str, permanent: bool) {
        let ip_only = extract_ip(peer_ip).to_string();
        let mut incompatible = self.incompatible_peers.write().await;

        // Check if already marked
        if !incompatible.contains_key(&ip_only) {
            tracing::error!(
                "🚫 ═══════════════════════════════════════════════════════════════════"
            );
            tracing::error!("🚫 INCOMPATIBLE PEER DETECTED: {}", ip_only);
            tracing::error!("🚫 Reason: {}", reason);
            tracing::error!("🚫 ");
            tracing::error!("🚫 This peer is computing different block hashes, likely due to");
            tracing::error!("🚫 running an older version of the software.");
            tracing::error!("🚫 ");
            tracing::error!("🚫 RECOMMENDATION: The peer should update to the latest version");
            tracing::error!("🚫 and clear their blockchain to resync.");
            tracing::error!("🚫 ");
            if permanent {
                tracing::error!(
                    "🚫 This is a GENESIS MISMATCH - peer will be PERMANENTLY ignored."
                );
            } else {
                tracing::error!(
                    "🚫 This peer will be temporarily ignored for {} minutes, then re-checked.",
                    Self::INCOMPATIBLE_RECHECK_SECS / 60
                );
            }
            tracing::error!(
                "🚫 ═══════════════════════════════════════════════════════════════════"
            );
        }

        incompatible.insert(
            ip_only,
            (std::time::Instant::now(), reason.to_string(), permanent),
        );
    }

    /// Check if a peer is marked as incompatible (with automatic expiry for non-permanent)
    pub async fn is_incompatible(&self, peer_ip: &str) -> bool {
        let ip_only = extract_ip(peer_ip);
        let incompatible = self.incompatible_peers.read().await;

        if let Some((marked_at, _reason, permanent)) = incompatible.get(ip_only) {
            // Permanent incompatibility (genesis mismatch) - NEVER expires
            if *permanent {
                return true;
            }

            // Check if enough time has passed to re-check
            if marked_at.elapsed().as_secs() >= Self::INCOMPATIBLE_RECHECK_SECS {
                // Time to re-check - return false to allow retry
                drop(incompatible);
                // Clear the entry so they get a fresh chance
                self.incompatible_peers.write().await.remove(ip_only);
                tracing::info!(
                    "🔄 Re-checking previously incompatible peer {} ({}min timeout expired)",
                    ip_only,
                    Self::INCOMPATIBLE_RECHECK_SECS / 60
                );
                return false;
            }
            true
        } else {
            false
        }
    }

    /// Clear incompatible status for a peer (when they resync or update)
    pub async fn clear_incompatible(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip).to_string();
        if self
            .incompatible_peers
            .write()
            .await
            .remove(&ip_only)
            .is_some()
        {
            tracing::info!("✅ Peer {} is now compatible - blocks accepted", ip_only);
        }
    }

    /// Threshold of persistent fork errors before triggering deep fork resolution
    /// Note: Fork errors alone do NOT mean incompatibility - only genesis hash mismatch does
    const FORK_ERROR_THRESHOLD: u32 = 3;

    /// Record a fork error for a peer (persistent across requests)
    /// Returns true if fork resolution should be triggered (NOT incompatibility)
    ///
    /// IMPORTANT: Fork errors are NORMAL when nodes are on different forks of the same chain.
    /// This does NOT mark peers as incompatible - only genesis hash mismatch does that.
    /// Instead, this triggers fork resolution to find common ancestor and reconcile.
    pub async fn record_fork_error(&self, peer_ip: &str) -> bool {
        let ip_only = extract_ip(peer_ip).to_string();

        // Increment the error count
        let count = self
            .fork_error_counts
            .entry(ip_only.clone())
            .and_modify(|c| *c += 1)
            .or_insert(1);

        let current_count = *count;

        if current_count >= Self::FORK_ERROR_THRESHOLD {
            // Don't mark as incompatible - forks are normal!
            // Instead, log that deep fork resolution is needed
            tracing::warn!(
                "🔀 Persistent fork with peer {} ({} errors) - needs fork resolution (finding common ancestor)",
                ip_only,
                current_count
            );
            // Return true to signal that fork resolution should be triggered
            // But do NOT mark as incompatible - that's only for genesis hash mismatch
            true
        } else {
            tracing::info!(
                "🔀 Fork error {} of {} for peer {} (will trigger resolution at threshold)",
                current_count,
                Self::FORK_ERROR_THRESHOLD,
                ip_only
            );
            false
        }
    }

    /// Mark a peer as truly incompatible (different software/hashing algorithm)
    /// This should ONLY be called when genesis hash doesn't match
    pub async fn mark_genesis_incompatible(
        &self,
        peer_ip: &str,
        our_genesis: &str,
        their_genesis: &str,
    ) {
        let ip_only = extract_ip(peer_ip).to_string();
        let mut incompatible = self.incompatible_peers.write().await;

        if !incompatible.contains_key(&ip_only) {
            tracing::error!(
                "🚫 ═══════════════════════════════════════════════════════════════════"
            );
            tracing::error!("🚫 INCOMPATIBLE PEER DETECTED: {}", ip_only);
            tracing::error!("🚫 Reason: Genesis hash mismatch");
            tracing::error!("🚫   Our genesis:   {}", our_genesis);
            tracing::error!("🚫   Their genesis: {}", their_genesis);
            tracing::error!("🚫 ");
            tracing::error!("🚫 This peer is computing different block hashes, likely due to");
            tracing::error!("🚫 running an older version of the software.");
            tracing::error!("🚫 ");
            tracing::error!("🚫 RECOMMENDATION: The peer should update to the latest version");
            tracing::error!("🚫 and clear their blockchain to resync.");
            tracing::error!("🚫 ");
            tracing::error!("🚫 Genesis mismatch is PERMANENT - peer will NEVER be rechecked.");
            tracing::error!(
                "🚫 ═══════════════════════════════════════════════════════════════════"
            );
        }

        let reason = format!(
            "Genesis hash mismatch: ours={}, theirs={}",
            our_genesis, their_genesis
        );
        // Genesis mismatch is PERMANENT - these peers will never sync correctly
        incompatible.insert(ip_only, (std::time::Instant::now(), reason, true));
    }

    /// Verify genesis hash compatibility with a peer
    /// Returns true if compatible (same genesis hash), false if incompatible
    /// If incompatible, marks the peer as such
    pub async fn verify_genesis_compatibility(
        &self,
        peer_ip: &str,
        our_genesis_hash: [u8; 32],
    ) -> bool {
        let ip_only = extract_ip(peer_ip);

        // Request the peer's genesis block hash
        let request = NetworkMessage::GetBlockHash(0);

        match self.send_and_await_response(peer_ip, request, 10).await {
            Ok(NetworkMessage::BlockHashResponse {
                height: 0,
                hash: Some(their_hash),
            }) => {
                if our_genesis_hash == their_hash {
                    tracing::info!(
                        "✅ Genesis hash matches with peer {} - compatible for fork resolution",
                        ip_only
                    );
                    // Reset fork errors since they're compatible
                    self.reset_fork_errors(peer_ip);
                    true
                } else {
                    let our_hex = hex::encode(&our_genesis_hash[..8]);
                    let their_hex = hex::encode(&their_hash[..8]);

                    tracing::error!(
                        "🚫 Genesis hash MISMATCH with peer {} - incompatible software!",
                        ip_only
                    );
                    tracing::error!("🚫   Our genesis:   {}...", our_hex);
                    tracing::error!("🚫   Their genesis: {}...", their_hex);

                    // Mark as truly incompatible
                    self.mark_genesis_incompatible(peer_ip, &our_hex, &their_hex)
                        .await;
                    false
                }
            }
            Ok(NetworkMessage::BlockHashResponse {
                height: 0,
                hash: None,
            }) => {
                tracing::warn!(
                    "⚠️ Peer {} has no genesis block - skipping compatibility check",
                    ip_only
                );
                // Can't verify, assume compatible for now
                true
            }
            Ok(other) => {
                tracing::warn!(
                    "⚠️ Unexpected response from {} for genesis hash: {:?}",
                    ip_only,
                    other.message_type()
                );
                // Can't verify, assume compatible for now
                true
            }
            Err(e) => {
                tracing::warn!(
                    "⚠️ Failed to get genesis hash from {}: {} - assuming compatible",
                    ip_only,
                    e
                );
                // Can't verify, assume compatible for now
                true
            }
        }
    }

    /// Reset fork error count for a peer (called when blocks are successfully added)
    pub fn reset_fork_errors(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip);
        if self.fork_error_counts.remove(ip_only).is_some() {
            tracing::debug!(
                "Reset fork error count for peer {} (blocks accepted)",
                ip_only
            );
        }
    }

    /// Increment fork error count and return the new count
    pub fn increment_fork_errors(&self, peer_ip: &str) -> u32 {
        let ip_only = extract_ip(peer_ip).to_string();
        let count = self
            .fork_error_counts
            .entry(ip_only)
            .and_modify(|c| *c += 1)
            .or_insert(1);
        *count
    }

    /// Get list of whitelisted peer IPs
    pub fn get_whitelisted_peers(&self) -> Vec<String> {
        // For now, return empty vec since whitelisting is checked per-peer
        // In the future, could maintain a cached list
        vec![]
    }

    /// Get list of compatible connected peers (excludes currently incompatible ones)
    pub async fn get_compatible_peers(&self) -> Vec<String> {
        // Return cached result if still fresh
        {
            let cache = self.compatible_peers_cache.read().await;
            if cache.1.elapsed().as_secs() < Self::COMPATIBLE_PEERS_CACHE_TTL && !cache.0.is_empty()
            {
                return cache.0.clone();
            }
        }

        // Cache miss — recompute
        let result = self.get_compatible_peers_uncached().await;

        // Store in cache
        {
            let mut cache = self.compatible_peers_cache.write().await;
            *cache = (result.clone(), std::time::Instant::now());
        }

        result
    }

    /// Uncached implementation of compatible peer computation
    async fn get_compatible_peers_uncached(&self) -> Vec<String> {
        // First, clean up expired incompatible entries (but NOT permanent ones)
        {
            let mut incompatible = self.incompatible_peers.write().await;
            incompatible.retain(|ip, (marked_at, _reason, permanent)| {
                // Permanent entries (genesis mismatch) are NEVER cleaned up
                if *permanent {
                    return true;
                }
                let expired = marked_at.elapsed().as_secs() >= Self::INCOMPATIBLE_RECHECK_SECS;
                if expired {
                    tracing::info!("🔄 Incompatible timeout expired for {}, will re-check", ip);
                }
                !expired
            });
        }

        let incompatible = self.incompatible_peers.read().await;
        let all_connections: Vec<String> =
            self.connections.iter().map(|e| e.key().clone()).collect();
        let compatible: Vec<String> = all_connections
            .iter()
            .filter(|ip| !incompatible.contains_key(extract_ip(ip)))
            .cloned()
            .collect();

        // Rate-limited logging for incompatible peers (once per 60 seconds)
        if !incompatible.is_empty() {
            static LAST_INCOMPATIBLE_LOG: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let last_log = LAST_INCOMPATIBLE_LOG.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last_log >= 60 {
                LAST_INCOMPATIBLE_LOG.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    "⚠️ Incompatible peers: {} marked, {} in connections, {} compatible",
                    incompatible.len(),
                    all_connections.len(),
                    compatible.len()
                );
                for (ip, (marked_at, reason, permanent)) in incompatible.iter() {
                    let status = if *permanent { "PERMANENT" } else { "temporary" };
                    tracing::warn!(
                        "  🚫 {} - {} [{}] ({}s ago)",
                        ip,
                        reason,
                        status,
                        marked_at.elapsed().as_secs()
                    );
                }
            }
        }

        compatible
    }

    /// Get count of incompatible peers (for monitoring)
    pub async fn incompatible_count(&self) -> usize {
        self.incompatible_peers.read().await.len()
    }

    /// Set TimeLock consensus resources (called once after server initialization)
    pub async fn set_timelock_resources(
        &self,
        consensus: Arc<ConsensusEngine>,
        block_cache: Arc<crate::network::block_cache::BlockCache>,
        broadcast_tx: broadcast::Sender<NetworkMessage>,
    ) {
        *self.timelock_consensus.write().await = Some(consensus);
        *self.timelock_block_cache.write().await = Some(block_cache);
        *self.timelock_broadcast.write().await = Some(broadcast_tx);
    }

    /// Get TimeLock consensus resources for message handling
    pub async fn get_timelock_resources(
        &self,
    ) -> (
        Option<Arc<ConsensusEngine>>,
        Option<Arc<crate::network::block_cache::BlockCache>>,
        Option<broadcast::Sender<NetworkMessage>>,
    ) {
        (
            self.timelock_consensus.read().await.clone(),
            self.timelock_block_cache.read().await.clone(),
            self.timelock_broadcast
                .read()
                .await
                .as_ref()
                .map(|tx| tx.clone()),
        )
    }

    /// Set WebSocket transaction event sender for real-time wallet notifications
    pub async fn set_tx_event_sender(
        &self,
        sender: broadcast::Sender<crate::rpc::websocket::TransactionEvent>,
    ) {
        *self.ws_tx_event_sender.write().await = Some(sender);
    }

    /// Get WebSocket transaction event sender
    pub async fn get_tx_event_sender(
        &self,
    ) -> Option<broadcast::Sender<crate::rpc::websocket::TransactionEvent>> {
        self.ws_tx_event_sender.read().await.clone()
    }

    // ===== Connection Direction Logic =====

    pub fn set_local_ip(&self, ip: String) {
        self.local_ip.store(Some(Arc::new(ip)));
    }

    pub fn get_local_ip(&self) -> Option<String> {
        self.local_ip
            .load()
            .as_ref()
            .map(|arc| arc.as_ref().clone())
    }

    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        let local_ip_guard = self.local_ip.load();

        if let Some(local_ip_arc) = local_ip_guard.as_ref() {
            let local_ip = local_ip_arc.as_str();

            if let (Ok(local_addr), Ok(peer_addr)) =
                (local_ip.parse::<IpAddr>(), peer_ip.parse::<IpAddr>())
            {
                match (local_addr, peer_addr) {
                    (IpAddr::V4(l), IpAddr::V4(p)) => l.octets() > p.octets(),
                    (IpAddr::V6(l), IpAddr::V6(p)) => l.octets() > p.octets(),
                    (IpAddr::V6(_), IpAddr::V4(_)) => true,
                    (IpAddr::V4(_), IpAddr::V6(_)) => false,
                }
            } else {
                local_ip > peer_ip
            }
        } else {
            true
        }
    }

    // ===== Connection State Management =====

    /// Atomically register inbound connection if not already connected
    /// Returns true if registration succeeded, false if already exists
    /// This prevents race conditions during concurrent connection attempts.
    /// Uses IP-based tiebreaker for simultaneous connections: the node with
    /// the lower IP keeps its outbound; the higher IP yields and accepts inbound.
    pub fn try_register_inbound(&self, ip: &str) -> bool {
        use dashmap::mapref::entry::Entry;

        match self.connections.entry(ip.to_string()) {
            Entry::Vacant(e) => {
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Inbound,
                    connected_at: Instant::now(),
                });
                self.inbound_count.fetch_add(1, Ordering::Relaxed);
                true
            }
            Entry::Occupied(mut e) => {
                if e.get().direction == ConnectionDirection::Outbound {
                    // Simultaneous connection: we have outbound, peer is sending inbound.
                    // Use deterministic tiebreaker: node with higher IP yields its outbound
                    // and accepts inbound instead. This prevents the reconnect loop where
                    // rejecting the inbound causes the peer to close our outbound too.
                    if !self.should_connect_to(ip) {
                        // Our IP is lower — we should yield outbound, accept inbound
                        tracing::info!(
                            "🔄 Simultaneous connection with {} — yielding outbound, accepting inbound (IP tiebreaker)",
                            ip
                        );
                        self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                        e.insert(ConnectionState {
                            direction: ConnectionDirection::Inbound,
                            connected_at: Instant::now(),
                        });
                        self.inbound_count.fetch_add(1, Ordering::Relaxed);
                        true
                    } else {
                        // Our IP is higher — keep outbound, reject inbound
                        tracing::debug!(
                            "🔄 Rejecting inbound from {} - outbound connection preferred (IP tiebreaker)",
                            ip
                        );
                        false
                    }
                } else {
                    // Already have an inbound connection, reject duplicate
                    tracing::debug!(
                        "🔄 Rejecting duplicate inbound from {} - already connected",
                        ip
                    );
                    false
                }
            }
        }
    }

    pub fn mark_connecting(&self, ip: &str) -> bool {
        use dashmap::mapref::entry::Entry;

        match self.connections.entry(ip.to_string()) {
            Entry::Vacant(e) => {
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Outbound,
                    connected_at: Instant::now(),
                });
                self.outbound_count.fetch_add(1, Ordering::Relaxed);
                true
            }
            Entry::Occupied(_) => {
                // Reject if any connection (inbound or outbound) already exists.
                // Prevents outbound from racing with an active inbound connection
                // and corrupting the writer channel.
                tracing::debug!(
                    "🔄 Rejecting outbound to {} - connection already exists",
                    ip
                );
                false
            }
        }
    }

    pub fn is_connected(&self, ip: &str) -> bool {
        self.connections.contains_key(ip)
    }

    /// Check if the current connection to a peer is outbound.
    /// Returns false if not connected or if the connection is inbound.
    pub fn is_outbound(&self, ip: &str) -> bool {
        self.connections
            .get(ip)
            .map(|s| s.direction == ConnectionDirection::Outbound)
            .unwrap_or(false)
    }

    pub fn mark_inbound(&self, ip: &str) -> bool {
        use dashmap::mapref::entry::Entry;

        match self.connections.entry(ip.to_string()) {
            Entry::Vacant(e) => {
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Inbound,
                    connected_at: Instant::now(),
                });
                self.inbound_count.fetch_add(1, Ordering::Relaxed);
                true
            }
            Entry::Occupied(mut e) => {
                // Allow reconnection by updating existing entry
                let old_direction = e.get().direction;
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Inbound,
                    connected_at: Instant::now(),
                });
                // Adjust counters if direction changed
                if old_direction == ConnectionDirection::Outbound {
                    self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                    self.inbound_count.fetch_add(1, Ordering::Relaxed);
                }
                true
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_direction(&self, ip: &str) -> Option<ConnectionDirection> {
        self.connections.get(ip).map(|e| e.direction)
    }

    pub fn mark_disconnected(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            match state.direction {
                ConnectionDirection::Inbound => {
                    self.inbound_count.fetch_sub(1, Ordering::Relaxed);
                }
                ConnectionDirection::Outbound => {
                    self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                }
            }
            // Clean up stale peer metadata
            tokio::spawn({
                let peer_chain_tips = Arc::clone(&self.peer_chain_tips);
                let peer_heights = Arc::clone(&self.peer_heights);
                let peer_ping_times = Arc::clone(&self.peer_ping_times);
                let ip = ip.to_string();
                async move {
                    peer_chain_tips.write().await.remove(&ip);
                    peer_heights.write().await.remove(&ip);
                    peer_ping_times.write().await.remove(&ip);
                }
            });
        }
    }

    pub fn remove(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            match state.direction {
                ConnectionDirection::Inbound => {
                    self.inbound_count.fetch_sub(1, Ordering::Relaxed);
                }
                ConnectionDirection::Outbound => {
                    self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                }
            }
            // Clean up stale peer metadata
            tokio::spawn({
                let peer_chain_tips = Arc::clone(&self.peer_chain_tips);
                let peer_heights = Arc::clone(&self.peer_heights);
                let peer_ping_times = Arc::clone(&self.peer_ping_times);
                let ip = ip.to_string();
                async move {
                    peer_chain_tips.write().await.remove(&ip);
                    peer_heights.write().await.remove(&ip);
                    peer_ping_times.write().await.remove(&ip);
                }
            });
        }
    }

    pub fn mark_inbound_disconnected(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            if state.direction == ConnectionDirection::Inbound {
                self.inbound_count.fetch_sub(1, Ordering::Relaxed);
            }
            // Clean up stale peer metadata
            tokio::spawn({
                let peer_chain_tips = Arc::clone(&self.peer_chain_tips);
                let peer_heights = Arc::clone(&self.peer_heights);
                let peer_ping_times = Arc::clone(&self.peer_ping_times);
                let ip = ip.to_string();
                async move {
                    peer_chain_tips.write().await.remove(&ip);
                    peer_heights.write().await.remove(&ip);
                    peer_ping_times.write().await.remove(&ip);
                }
            });
        }
    }

    pub fn connected_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed) + self.outbound_count.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn inbound_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn outbound_count(&self) -> usize {
        self.outbound_count.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn mark_reconnecting(&self, ip: &str, retry_delay: u64, attempt_count: u64) {
        self.reconnecting.insert(
            ip.to_string(),
            ReconnectionState {
                next_attempt: Instant::now() + std::time::Duration::from_secs(retry_delay),
                attempt_count,
            },
        );
    }

    pub fn is_reconnecting(&self, ip: &str) -> bool {
        if let Some(state) = self.reconnecting.get(ip) {
            Instant::now() < state.next_attempt
        } else {
            false
        }
    }

    pub fn clear_reconnecting(&self, ip: &str) {
        self.reconnecting.remove(ip);
    }

    #[allow(dead_code)]
    pub fn cleanup_reconnecting(&self, max_age: std::time::Duration) {
        let now = Instant::now();
        self.reconnecting.retain(|_, state| {
            now < state.next_attempt || now.duration_since(state.next_attempt) < max_age
        });
    }

    // ===== Peer Writer Registry (formerly peer_connection_registry.rs) =====

    pub async fn register_peer(&self, peer_ip: String, writer: PeerWriterTx) {
        // Mark as connected in the connections map for get_connected_peers()
        self.mark_inbound(&peer_ip);

        let mut writers = self.peer_writers.write().await;
        // Only overwrite if no existing live writer (defensive, mirrors register_peer_shared)
        if let Some(existing) = writers.get(&peer_ip) {
            if !existing.is_closed() {
                debug!(
                    "🔄 Inbound peer {} already has a live writer, skipping overwrite",
                    peer_ip
                );
                return;
            }
            debug!("♻️ Replacing dead writer for inbound peer {}", peer_ip);
        }
        writers.insert(peer_ip.clone(), writer);
        debug!("✅ Registered peer connection: {}", peer_ip);
    }

    /// Register an outbound peer with a channel-based writer
    pub async fn register_peer_shared(&self, peer_ip: String, writer: SharedPeerWriter) {
        // Also mark as connected in the connections map for get_connected_peers()
        self.mark_connecting(&peer_ip);

        let mut writers = self.peer_writers.write().await;
        // Only overwrite if the existing writer is dead (channel closed).
        // A live writer (e.g., from an accepted inbound connection) must not be
        // replaced by a speculative outbound writer that may be rejected.
        if let Some(existing) = writers.get(&peer_ip) {
            if !existing.is_closed() {
                debug!(
                    "🔄 Outbound peer {} already has a live writer, skipping overwrite",
                    peer_ip
                );
                return;
            }
            debug!("♻️ Replacing dead writer for peer {}", peer_ip);
        }
        writers.insert(peer_ip.clone(), writer);
        debug!("✅ Registered outbound peer connection: {}", peer_ip);
    }

    pub async fn unregister_peer(&self, peer_ip: &str) {
        // Remove from connections map
        self.mark_disconnected(peer_ip);

        let mut writers = self.peer_writers.write().await;
        writers.remove(peer_ip);
        debug!("🔌 Unregistered peer connection: {}", peer_ip);

        let mut pending = self.pending_responses.write().await;
        pending.remove(peer_ip);

        // Remove peer height and ping time
        let mut heights = self.peer_heights.write().await;
        heights.remove(peer_ip);
        let mut ping_times = self.peer_ping_times.write().await;
        ping_times.remove(peer_ip);
    }

    /// Set a peer's reported blockchain height
    pub async fn set_peer_height(&self, peer_ip: &str, height: u64) {
        let ip_only = extract_ip(peer_ip);
        let mut heights = self.peer_heights.write().await;
        heights.insert(ip_only.to_string(), height);
    }

    /// Get a peer's reported blockchain height
    pub async fn get_peer_height(&self, peer_ip: &str) -> Option<u64> {
        let ip_only = extract_ip(peer_ip);
        let heights = self.peer_heights.read().await;
        heights.get(ip_only).copied()
    }

    /// Set a peer's latest ping RTT in seconds
    pub async fn set_peer_ping_time(&self, peer_ip: &str, rtt_secs: f64) {
        let ip_only = extract_ip(peer_ip);
        let mut times = self.peer_ping_times.write().await;
        times.insert(ip_only.to_string(), rtt_secs);
    }

    /// Get a peer's latest ping RTT in seconds
    pub async fn get_peer_ping_time(&self, peer_ip: &str) -> Option<f64> {
        let ip_only = extract_ip(peer_ip);
        let times = self.peer_ping_times.read().await;
        times.get(ip_only).copied()
    }

    /// Phase 3: Update a peer's known height
    pub async fn update_peer_height(&self, peer_ip: &str, height: u64) {
        let ip_only = extract_ip(peer_ip);
        let mut heights = self.peer_heights.write().await;
        heights.insert(ip_only.to_string(), height);
    }

    /// Update a peer's chain tip (height + hash)
    /// Only updates if the new height is >= the cached height (monotonic),
    /// preventing stale ChainTipResponse from overwriting a newer forced update.
    pub async fn update_peer_chain_tip(&self, peer_ip: &str, height: u64, hash: [u8; 32]) {
        let ip_only = extract_ip(peer_ip);
        let mut tips = self.peer_chain_tips.write().await;
        if let Some(&(existing_height, _)) = tips.get(ip_only) {
            if height < existing_height {
                tracing::debug!(
                    "🔄 Ignoring stale chain tip for {} (cached: {}, received: {})",
                    ip_only,
                    existing_height,
                    height
                );
                return;
            }
        }
        tips.insert(ip_only.to_string(), (height, hash));
        drop(tips);
        self.chain_tip_updated.notify_waiters();
    }

    /// Get a peer's chain tip (height + hash)
    pub async fn get_peer_chain_tip(&self, peer_ip: &str) -> Option<ChainTip> {
        let ip_only = extract_ip(peer_ip);
        let tips = self.peer_chain_tips.read().await;
        tips.get(ip_only).copied()
    }

    /// Get the chain tip update signal (notified when any peer reports a new chain tip)
    pub fn chain_tip_updated_signal(&self) -> Arc<tokio::sync::Notify> {
        self.chain_tip_updated.clone()
    }

    /// Clear stale peer data when peer disconnects
    pub async fn clear_peer_data(&self, peer_ip: &str) {
        let mut heights = self.peer_heights.write().await;
        let mut tips = self.peer_chain_tips.write().await;
        let mut ping_times = self.peer_ping_times.write().await;
        heights.remove(peer_ip);
        tips.remove(peer_ip);
        ping_times.remove(peer_ip);
        tracing::debug!(
            "🧹 Cleared stale chain tip data for disconnected peer {}",
            peer_ip
        );
    }

    pub async fn get_peer_writer(&self, peer_ip: &str) -> Option<PeerWriterTx> {
        let ip_only = extract_ip(peer_ip);
        let writers = self.peer_writers.read().await;
        writers.get(ip_only).cloned()
    }

    pub async fn register_response_handler(&self, peer_ip: &str, tx: ResponseSender) {
        let mut pending = self.pending_responses.write().await;
        pending
            .entry(peer_ip.to_string())
            .or_insert_with(Vec::new)
            .push(tx);
    }

    pub async fn get_response_handlers(&self, peer_ip: &str) -> Option<Vec<ResponseSender>> {
        let mut pending = self.pending_responses.write().await;
        pending.remove(peer_ip)
    }

    pub async fn list_peers(&self) -> Vec<String> {
        let writers = self.peer_writers.read().await;
        writers.keys().cloned().collect()
    }

    /// Send a message to a specific peer
    pub async fn send_to_peer(&self, peer_ip: &str, message: NetworkMessage) -> Result<(), String> {
        // Extract IP only (remove port if present)
        let ip_only = extract_ip(peer_ip);

        let writers = self.peer_writers.read().await;

        if let Some(writer_tx) = writers.get(ip_only) {
            // Pre-serialize the message into a length-prefixed frame
            let frame_bytes = crate::network::wire::serialize_frame(&message)?;

            writer_tx
                .send(frame_bytes)
                .map_err(|_| format!("Writer channel closed for peer {}", ip_only))?;

            Ok(())
        } else {
            tracing::debug!(
                "❌ Peer {} not found in registry (available: {:?})",
                ip_only,
                writers.keys().collect::<Vec<_>>()
            );
            Err(format!("Peer {} not connected", ip_only))
        }
    }

    /// Send a message to a peer and wait for a response
    pub async fn send_and_await_response(
        &self,
        peer_ip: &str,
        message: NetworkMessage,
        timeout_secs: u64,
    ) -> Result<NetworkMessage, String> {
        // Extract IP only
        let ip_only = extract_ip(peer_ip);
        let (tx, rx) = oneshot::channel();

        // Register pending response
        {
            let mut pending = self.pending_responses.write().await;
            pending
                .entry(ip_only.to_string())
                .or_insert_with(Vec::new)
                .push(tx);
        }

        // Send the message
        self.send_to_peer(ip_only, message).await?;

        // Wait for response with timeout
        match tokio::time::timeout(tokio::time::Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err("Response channel closed".to_string()),
            Err(_) => {
                // Clean up pending response on timeout
                let mut pending = self.pending_responses.write().await;
                if let Some(senders) = pending.get_mut(ip_only) {
                    senders.retain(|_| false); // Remove all pending for simplicity
                }
                Err(format!("Timeout waiting for response from {}", peer_ip))
            }
        }
    }

    /// Handle an incoming response message (called from message loop)
    pub async fn handle_response(&self, peer_ip: &str, message: NetworkMessage) {
        // Extract IP only
        let ip_only = extract_ip(peer_ip);
        let mut pending = self.pending_responses.write().await;

        if let Some(senders) = pending.get_mut(ip_only) {
            if let Some(sender) = senders.pop() {
                if sender.send(message).is_err() {
                    warn!(
                        "Failed to send response to awaiting task for peer {}",
                        ip_only
                    );
                }
            }
        }
    }

    /// Broadcast a message to all connected peers (pre-serializes for efficiency)
    pub async fn broadcast(&self, message: NetworkMessage) {
        let writers = self.peer_writers.read().await;

        if writers.is_empty() {
            // Rate-limit this warning to avoid log spam during bootstrap
            static LAST_NO_PEERS_BCAST: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = chrono::Utc::now().timestamp();
            let last = LAST_NO_PEERS_BCAST.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last >= 60 {
                LAST_NO_PEERS_BCAST.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                warn!("📡 Broadcast: no peers connected!");
            }
            return;
        }

        // Pre-serialize the message once for efficiency
        let msg_bytes = match crate::network::wire::serialize_frame(&message) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!("❌ Failed to serialize broadcast message: {}", e);
                return;
            }
        };

        let mut send_count = 0;
        let mut fail_count = 0;

        // Log for transaction broadcasts
        let is_tx_broadcast = matches!(message, NetworkMessage::TransactionBroadcast(_));

        for (peer_ip, writer_tx) in writers.iter() {
            if let Err(_e) = writer_tx.send(msg_bytes.clone()) {
                if is_tx_broadcast {
                    warn!("❌ TX broadcast to {} failed: channel closed", peer_ip);
                } else {
                    debug!("❌ Broadcast to {} failed: channel closed", peer_ip);
                }
                fail_count += 1;
                continue;
            }

            send_count += 1;
        }

        if is_tx_broadcast {
            info!(
                "📡 TX broadcast complete: {} peers sent, {} failed",
                send_count, fail_count
            );
        } else if send_count > 0 || fail_count > 0 {
            debug!(
                "📡 Broadcast complete: {} sent, {} failed",
                send_count, fail_count
            );
        }
    }

    /// Get list of connected peer IPs
    /// Only returns peers that have completed the handshake and have an active
    /// writer channel — i.e. truly connected, not just in the Connecting state.
    pub async fn get_connected_peers(&self) -> Vec<String> {
        let writers = self.peer_writers.read().await;
        self.connections
            .iter()
            .filter(|entry| {
                // Only include peers that have a live writer (post-handshake)
                writers
                    .get(entry.key())
                    .map(|w| !w.is_closed())
                    .unwrap_or(false)
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get count of connected peers (post-handshake only)
    pub async fn peer_count(&self) -> usize {
        let writers = self.peer_writers.read().await;
        self.connections
            .iter()
            .filter(|entry| {
                writers
                    .get(entry.key())
                    .map(|w| !w.is_closed())
                    .unwrap_or(false)
            })
            .count()
    }

    /// Get a snapshot of connected peer IPs (for stats/monitoring)
    #[allow(dead_code)]
    pub async fn get_connected_peers_list(&self) -> Vec<String> {
        self.connections
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get statistics about pending responses (for monitoring)
    #[allow(dead_code)]
    pub async fn pending_response_count(&self) -> usize {
        let pending = self.pending_responses.read().await;
        pending.values().map(|senders| senders.len()).sum()
    }

    /// Send multiple messages to a peer in a batch (more efficient than individual sends)
    #[allow(dead_code)]
    pub async fn send_batch_to_peer(
        &self,
        peer_ip: &str,
        _messages: &[NetworkMessage],
    ) -> Result<(), String> {
        let ip_only = extract_ip(peer_ip);

        let writers = self.peer_writers.read().await;
        if !writers.contains_key(ip_only) {
            tracing::debug!("❌ Peer {} not found in registry", ip_only);
            return Err(format!("Peer {} not connected", ip_only));
        }

        // Message sending is handled by the network server
        // This is a placeholder for the refactored architecture
        Ok(())
    }

    /// Broadcast multiple messages to all connected peers efficiently
    #[allow(dead_code)]
    pub async fn broadcast_batch(&self, _messages: &[NetworkMessage]) {
        // Batch broadcasting is handled by the network server
        // This is a placeholder for the refactored architecture
        debug!("📡 Broadcast batch called (message routing handled by server)");
    }

    /// Selective gossip: send to random subset of peers to reduce bandwidth
    /// Default fan-out: 20 peers (configurable)
    #[allow(dead_code)]
    pub async fn gossip_selective(
        &self,
        message: NetworkMessage,
        source_peer: Option<&str>,
    ) -> usize {
        self.gossip_selective_with_config(message, source_peer, 20)
            .await
    }

    /// Selective gossip with configurable fan-out
    /// Returns number of peers message was sent to
    #[allow(dead_code)]
    pub async fn gossip_selective_with_config(
        &self,
        _message: NetworkMessage,
        _source_peer: Option<&str>,
        fan_out: usize,
    ) -> usize {
        // Selective gossip is handled by the network server
        // Return the configured fan-out as indication
        fan_out
    }

    /// Add discovered peers from peer exchange
    pub async fn add_discovered_peers(&self, peers: &[String]) {
        let mut discovered = self.discovered_peers.write().await;
        let mut added = 0;
        for peer in peers {
            // Extract IP only (remove port if present)
            let ip = extract_ip(peer);
            if discovered.insert(ip.to_string()) {
                added += 1;
            }
        }
        if added > 0 {
            debug!("📥 Added {} new discovered peer candidate(s)", added);
        }
    }

    /// Get and clear discovered peers (for network client to process)
    pub async fn take_discovered_peers(&self) -> Vec<String> {
        let mut discovered = self.discovered_peers.write().await;
        let peers: Vec<String> = discovered.drain().collect();
        peers
    }

    /// Get discovered peers count
    pub async fn discovered_peers_count(&self) -> usize {
        self.discovered_peers.read().await.len()
    }

    /// Record the reported connection count for a peer (from PeerExchange messages).
    /// Used for load-aware peer selection so nodes can steer new connections toward
    /// less-loaded masternodes instead of always hitting the same bootstrap nodes.
    pub fn update_peer_load(&self, ip: &str, connection_count: u16) {
        self.peer_load.insert(ip.to_string(), connection_count);
    }

    /// Get the last-reported connection count for a peer, or u16::MAX if unknown.
    /// Returning MAX for unknown peers causes them to sort to the back, so we prefer
    /// known-underloaded peers while still eventually trying unknown ones.
    pub fn get_peer_load(&self, ip: &str) -> u16 {
        self.peer_load.get(ip).map(|v| *v).unwrap_or(u16::MAX)
    }

    /// Build a PeerExchange list of currently connected peers, sorted by ascending
    /// connection load, capped at `limit` entries.  Callers use this to respond to
    /// GetPeers requests and to redirect overloaded inbound connections.
    /// Note: `tier` is left as None here — callers with registry access fill it in.
    pub async fn get_peers_by_load(
        &self,
        limit: usize,
    ) -> Vec<crate::network::message::PeerExchangeEntry> {
        let connected = self.get_connected_peers().await;
        let mut entries: Vec<_> = connected
            .into_iter()
            .map(|ip| {
                let count = self.get_peer_load(&ip);
                let is_mn = self.peer_load.contains_key(ip.as_str());
                crate::network::message::PeerExchangeEntry {
                    address: ip,
                    connection_count: count,
                    is_masternode: is_mn,
                    tier: None, // filled in by callers that have masternode registry access
                }
            })
            .collect();
        // Sort ascending by load — least loaded first
        entries.sort_by_key(|e| e.connection_count);
        entries.truncate(limit);
        entries
    }
}

impl Default for PeerConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
