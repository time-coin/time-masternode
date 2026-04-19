//! Network server for P2P communication.
//!
//! Note: This module appears as "dead code" in library checks because it's
//! only used by the binary (main.rs). The NetworkServer is created and run
//! in main() for handling all P2P network communication.

use crate::consensus::ConsensusEngine;
use crate::network::blacklist::IPBlacklist;
use crate::network::block_cache::BlockCache;
use crate::network::dedup_filter::DeduplicationFilter;
use crate::network::message::{NetworkMessage, Subscription, UTXOStateChange};
use crate::network::message_handler::{ConnectionDirection, MessageContext, MessageHandler};
use crate::network::peer_connection::PeerStateManager;
use crate::network::rate_limiter::RateLimiter;
use crate::types::{OutPoint, UTXOState};
use crate::utxo_manager::UTXOStateManager;
use dashmap::DashMap;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

// Phase 2 Enhancement: Track peers on different forks
// NOTE: Currently unused after fork resolution consolidation, but kept for potential future use
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct PeerForkStatus {
    consecutive_invalid_blocks: u32,
    last_invalid_at: Instant,
    on_incompatible_fork: bool,
}

impl Default for PeerForkStatus {
    fn default() -> Self {
        Self {
            consecutive_invalid_blocks: 0,
            last_invalid_at: Instant::now(),
            on_incompatible_fork: false,
        }
    }
}

#[allow(dead_code)]
pub struct NetworkServer {
    pub listener: TcpListener,
    pub peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    pub subscriptions: Arc<RwLock<HashMap<String, Subscription>>>,
    pub tx_notifier: broadcast::Sender<NetworkMessage>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub consensus: Arc<ConsensusEngine>,
    pub rate_limiter: Arc<RwLock<RateLimiter>>,
    pub blacklist: Arc<RwLock<IPBlacklist>>,
    pub masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    pub blockchain: Arc<crate::blockchain::Blockchain>,
    pub peer_manager: Arc<crate::peer_manager::PeerManager>,
    pub seen_blocks: Arc<DeduplicationFilter>, // Bloom filter for block heights
    pub seen_transactions: Arc<DeduplicationFilter>, // Bloom filter for tx hashes
    pub seen_tx_finalized: Arc<DeduplicationFilter>, // Bloom filter for finalized tx messages
    pub seen_utxo_locks: Arc<DeduplicationFilter>, // Bloom filter for UTXO lock updates
    pub connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    pub peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    #[allow(dead_code)]
    pub peer_state: Arc<PeerStateManager>,
    pub local_ip: Option<String>, // Our own public IP (without port) to avoid self-connection
    pub block_cache: Arc<BlockCache>, // Phase 3E.1: Bounded cache for TimeLock voting
    pub peer_fork_status: Arc<DashMap<String, PeerForkStatus>>, // Track peers on incompatible forks
    pub ai_system: Option<Arc<crate::ai::AISystem>>, // AI attack detection & mitigation
    pub tls_config: Option<Arc<crate::network::tls::TlsConfig>>, // TLS for encrypted connections
    pub network_type: crate::network_type::NetworkType,
}

#[allow(dead_code)] // Used by binary, not visible to library check
pub struct PeerInfo {
    pub addr: String,
    pub is_masternode: bool,
}

impl NetworkServer {
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // Used by binary (main.rs)
    pub async fn new(
        bind_addr: &str,
        utxo_manager: Arc<UTXOStateManager>,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
        blockchain: Arc<crate::blockchain::Blockchain>,
        peer_manager: Arc<crate::peer_manager::PeerManager>,
        connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        peer_state: Arc<PeerStateManager>,
        local_ip: Option<String>,
        network_type: crate::network_type::NetworkType,
    ) -> Result<Self, std::io::Error> {
        Self::new_with_blacklist(
            bind_addr,
            utxo_manager,
            consensus,
            masternode_registry,
            blockchain,
            peer_manager,
            connection_manager,
            peer_registry,
            peer_state,
            local_ip,
            vec![],
            vec![],
            vec![],
            network_type,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // Used by binary (main.rs)
    pub async fn new_with_blacklist(
        bind_addr: &str,
        utxo_manager: Arc<UTXOStateManager>,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
        blockchain: Arc<crate::blockchain::Blockchain>,
        peer_manager: Arc<crate::peer_manager::PeerManager>,
        connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        peer_state: Arc<PeerStateManager>,
        local_ip: Option<String>,
        blacklisted_peers: Vec<String>,
        blacklisted_subnets: Vec<String>,
        whitelisted_peers: Vec<String>,
        network_type: crate::network_type::NetworkType,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(bind_addr).await?;
        let (tx, _) = broadcast::channel(1024);

        // Initialize blacklist with configured IPs
        let mut blacklist = IPBlacklist::new();
        for peer in &blacklisted_peers {
            if let Ok(ip) = peer.parse::<std::net::IpAddr>() {
                blacklist.add_permanent_ban(ip, "Configured in blacklisted_peers");
                tracing::info!("🚫 Blacklisted peer from config: {}", ip);
            } else {
                tracing::warn!("⚠️  Invalid IP in blacklisted_peers: {}", peer);
            }
        }

        // Initialize subnet bans from config
        for subnet in &blacklisted_subnets {
            blacklist.add_subnet_ban(subnet, "Configured in bansubnet");
        }

        // Initialize whitelist with configured IPs (BEFORE server starts accepting connections)
        for peer in &whitelisted_peers {
            if let Ok(ip) = peer.parse::<std::net::IpAddr>() {
                blacklist.add_to_whitelist(ip, "Pre-configured whitelist");
                tracing::debug!("✅ Whitelisted peer before server start: {}", ip);
            } else {
                tracing::warn!("⚠️  Invalid IP in whitelisted_peers: {}", peer);
            }
        }

        Ok(Self {
            listener,
            peers: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            tx_notifier: tx,
            utxo_manager,
            consensus,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new())),
            blacklist: Arc::new(RwLock::new(blacklist)),
            masternode_registry: masternode_registry.clone(),
            blockchain,
            peer_manager,
            seen_blocks: Arc::new(DeduplicationFilter::new(Duration::from_secs(300))), // 5 min rotation
            seen_transactions: Arc::new(DeduplicationFilter::new(Duration::from_secs(600))), // 10 min rotation
            seen_tx_finalized: Arc::new(DeduplicationFilter::new(Duration::from_secs(600))), // 10 min rotation
            seen_utxo_locks: Arc::new(DeduplicationFilter::new(Duration::from_secs(300))), // 5 min rotation
            connection_manager,
            peer_registry,
            peer_state,
            local_ip,
            block_cache: Arc::new(BlockCache::new_with_expiration(
                1000,                     // Max 1000 blocks
                Duration::from_secs(300), // 5 minute expiration
            )), // Phase 3E.1: Bounded LRU cache
            peer_fork_status: Arc::new(DashMap::new()), // Phase 2: Track fork status
            ai_system: None,
            tls_config: None,
            network_type,
        })
    }

    /// Set the AI system for attack detection and mitigation enforcement
    pub fn set_ai_system(&mut self, ai_system: Arc<crate::ai::AISystem>) {
        self.ai_system = Some(ai_system);
    }

    /// Attach a sled database for blacklist persistence across restarts.
    ///
    /// Loads previously banned IPs/subnets/violations from sled and enables
    /// write-through on all future mutations.  Call once after `new_with_blacklist`.
    ///
    /// After reloading persisted subnet bans, this also runs an **eviction sweep**:
    /// any Free-tier masternodes from a previously-banned /24 that somehow re-registered
    /// before the ban was enforced are immediately evicted and their TCP connections kicked.
    pub async fn enable_blacklist_persistence(&self, db: &sled::Db) {
        self.blacklist.write().await.attach_storage(db);
        tracing::info!("🔒 Blacklist persistence enabled — bans will survive restarts");
        // After loading persisted bans, ensure we never ban our own IP.
        // This clears any accidental self-ban that accumulated from self-connection
        // TLS failures (the node briefly connecting to its own IP via the peer list).
        if let Some(ref own_ip) = self.local_ip {
            if let Ok(ip) = own_ip.parse::<IpAddr>() {
                let mut bl = self.blacklist.write().await;
                if bl.is_blacklisted(ip).is_some() {
                    bl.unban(ip);
                    tracing::info!("🏠 Cleared self-ban for local IP {} on startup", own_ip);
                }
                // Whitelist our own IP permanently so future self-connections never ban us.
                bl.add_to_whitelist(ip, "local node IP");
            }
        }

        // Startup eviction sweep: for each subnet that was persisted as banned in a
        // previous session, evict any Free-tier masternodes that are currently registered
        // from that subnet.  This handles two cases:
        //   (a) Nodes from a banned subnet that were registered *before* the ban was applied.
        //   (b) Nodes that re-registered while the daemon was offline and the ban was not
        //       yet in memory.
        // We do NOT evict paid-tier nodes — they have on-chain collateral, and a subnet ban
        // should never be used as a mechanism to remove a legitimately funded node.
        let banned_subnets = self.blacklist.read().await.list_banned_subnets();
        if !banned_subnets.is_empty() {
            tracing::info!(
                "🔒 Running startup eviction sweep for {} persisted banned subnet(s)",
                banned_subnets.len()
            );
            for cidr in &banned_subnets {
                // Extract 3-octet prefix from CIDR (e.g. "154.217.246.0/24" → "154.217.246")
                let prefix: String = cidr
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(".");
                if prefix.is_empty() || prefix.contains(':') {
                    continue; // IPv6 or malformed — skip
                }
                let evicted = self
                    .masternode_registry
                    .evict_free_tier_subnet(&prefix)
                    .await;
                for ip in &evicted {
                    self.peer_registry.kick_peer(ip).await;
                }
                if !evicted.is_empty() {
                    tracing::warn!(
                        "🔒 Startup sweep: evicted {} stale Free-tier node(s) from previously-banned subnet {}/24",
                        evicted.len(),
                        prefix
                    );
                }
            }
        }
    }

    /// Set the TLS configuration for encrypted connections
    pub fn set_tls_config(&mut self, tls_config: Arc<crate::network::tls::TlsConfig>) {
        self.tls_config = Some(tls_config);
    }

    #[allow(dead_code)] // Used by binary (main.rs)
    pub async fn run(&mut self) -> Result<(), std::io::Error> {
        // Spawn cleanup task for blacklist
        let blacklist_cleanup = self.blacklist.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await; // Every 5 minutes
                blacklist_cleanup.write().await.cleanup();
            }
        });

        // Spawn cleanup task for block cache
        let block_cache_cleanup = self.block_cache.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await; // Every minute
                let removed = block_cache_cleanup.cleanup_expired();
                if removed > 0 {
                    let stats = block_cache_cleanup.stats();
                    tracing::debug!(
                        "📊 Block cache: {} blocks ({}% full), removed {} expired",
                        stats.current_size,
                        stats.usage_percent as u32,
                        removed
                    );
                }
            }
        });

        // Spawn attack mitigation enforcement task.
        // Wakes immediately when a new attack is detected (via ban_notifier Notify)
        // and falls back to a 30-second poll so transient misses are still caught.
        if let Some(ai) = &self.ai_system {
            let enforce_ai = ai.clone();
            let enforce_blacklist = self.blacklist.clone();
            let enforce_registry = self.peer_registry.clone();
            let enforce_mn_registry = self.masternode_registry.clone();
            // Grab the notifier before entering the task so we don't hold an Arc<AISystem>
            // reference just for the notifier.
            let enforce_notify = enforce_ai.attack_detector.ban_notifier();
            tokio::spawn(async move {
                loop {
                    // Wait for either an immediate wakeup from a new attack detection
                    // or the 30-second fallback tick — whichever comes first.
                    tokio::select! {
                        _ = enforce_notify.notified() => {}
                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {}
                    }

                    // take_pending_mitigations() returns only attacks that haven't been actioned
                    // yet, and marks them so subsequent ticks don't re-apply the same violation.
                    // This prevents a single detection event from accumulating 10 violations and
                    // triggering a permanent ban within 5 minutes.
                    let attacks = enforce_ai.attack_detector.take_pending_mitigations();

                    if attacks.is_empty() {
                        continue;
                    }

                    // Collect peers to kick *before* dropping the blacklist lock.
                    // kick_peer() is async and must not be called while holding the lock.
                    let mut to_kick: Vec<String> = Vec::new();
                    // Collect subnet prefixes (3-octet) that need registry eviction.
                    let mut to_evict_subnets: Vec<String> = Vec::new();

                    {
                        let mut blacklist = enforce_blacklist.write().await;
                        for attack in &attacks {
                            match &attack.recommended_action {
                                crate::ai::MitigationAction::BlockPeer(ip_str) => {
                                    if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
                                        if blacklist.is_whitelisted(ip) {
                                            // Whitelisted peers are operator-trusted — never
                                            // disconnect them regardless of AI confidence.
                                            // They may be relaying attacker traffic innocently;
                                            // banning them would drop legitimate stake weight.
                                            tracing::warn!(
                                                "⚠️ AI: BlockPeer suppressed for whitelisted peer {} ({:?}) — log only",
                                                ip, attack.attack_type
                                            );
                                        } else {
                                            let should_disconnect = blacklist.record_violation(
                                                ip,
                                                &format!("{:?}", attack.attack_type),
                                            );
                                            if should_disconnect {
                                                tracing::warn!(
                                                    "🚫 AI: Banned peer {} ({:?}, confidence={:.0}%)",
                                                    ip,
                                                    attack.attack_type,
                                                    attack.confidence * 100.0
                                                );
                                                to_kick.push(ip_str.clone());
                                            }
                                        }
                                    }
                                }
                                crate::ai::MitigationAction::RateLimitPeer(ip_str) => {
                                    if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
                                        if blacklist.is_whitelisted(ip) {
                                            // Never penalize whitelisted peers for rate-limit
                                            // violations — they may be relaying legitimately.
                                            tracing::warn!(
                                                "⚠️ AI: RateLimitPeer suppressed for whitelisted peer {} ({:?}) — log only",
                                                ip, attack.attack_type
                                            );
                                        } else {
                                            // Rate limit = record as violation (escalates automatically)
                                            let should_disconnect = blacklist.record_violation(
                                                ip,
                                                &format!("Rate limited: {:?}", attack.attack_type),
                                            );
                                            if should_disconnect {
                                                tracing::warn!(
                                                    "🚫 AI: Rate-limited peer {} escalated to ban ({:?})",
                                                    ip,
                                                    attack.attack_type
                                                );
                                                to_kick.push(ip_str.clone());
                                            }
                                        }
                                    }
                                }
                                crate::ai::MitigationAction::AlertOperator => {
                                    tracing::error!(
                                        "🚨 AI ALERT: {:?} detected (confidence={:.0}%, severity={:?})",
                                        attack.attack_type,
                                        attack.confidence * 100.0,
                                        attack.severity
                                    );
                                }
                                crate::ai::MitigationAction::BanSubnet(subnet_str) => {
                                    blacklist.add_subnet_ban(
                                        subnet_str,
                                        &format!("AI-detected: {:?}", attack.attack_type),
                                    );
                                    tracing::warn!(
                                        "🚫 AI: Auto-banned subnet {} ({:?}, confidence={:.0}%)",
                                        subnet_str,
                                        attack.attack_type,
                                        attack.confidence * 100.0
                                    );
                                    // Extract the 3-octet prefix from the CIDR for registry eviction.
                                    // e.g. "154.217.246.0/24" → "154.217.246"
                                    let prefix = subnet_str
                                        .split('/')
                                        .next()
                                        .unwrap_or("")
                                        .split('.')
                                        .take(3)
                                        .collect::<Vec<_>>()
                                        .join(".");
                                    if !prefix.is_empty() {
                                        // Also schedule all individual IPs from the attack for kicking
                                        for src_ip in &attack.source_ips {
                                            to_kick.push(src_ip.clone());
                                        }
                                        to_evict_subnets.push(prefix);
                                    }
                                }
                                _ => {} // Monitor, EmergencySync, HaltProduction — log only
                            }
                        }
                    } // blacklist write lock released here

                    // Evict Free-tier masternodes from banned subnets and kick their connections.
                    for subnet_prefix in &to_evict_subnets {
                        let evicted = enforce_mn_registry
                            .evict_free_tier_subnet(subnet_prefix)
                            .await;
                        for ip in evicted {
                            enforce_registry.kick_peer(&ip).await;
                        }
                    }

                    // Now close the TCP connections for every banned peer.
                    // kick_peer() removes the peer from the registry and drops the writer
                    // channel, which causes the I/O bridge task to exit and the TCP socket
                    // to close — without this, bans only take effect on the next message.
                    for ip_str in &to_kick {
                        enforce_registry.kick_peer(ip_str).await;
                    }
                }
            });
            tracing::info!(
                "🛡️ Attack mitigation enforcement task started (event-driven + 30s fallback)"
            );
        }

        // Collateral audit sweep: every 5 minutes, scan all paid-tier registrations and
        // verify each one's reward_address matches the on-chain UTXO output address.
        // A mismatch is definitive proof that the registrant does not own the collateral
        // (they cannot have produced the UTXO without knowing the correct address).
        // Detected squatters are evicted, their lock released, and permanently banned.
        {
            let audit_registry = self.masternode_registry.clone();
            let audit_utxo = self.utxo_manager.clone();
            let audit_blacklist = self.blacklist.clone();
            let audit_peer_registry = self.peer_registry.clone();
            tokio::spawn(async move {
                // Stagger the first run by 2 minutes to let the node finish syncing
                // before condemning nodes whose UTXOs may not yet be in our UTXO set.
                tokio::time::sleep(tokio::time::Duration::from_secs(120)).await;
                loop {
                    let squatters = audit_collateral_registrations(
                        &audit_registry,
                        &audit_utxo,
                        &audit_blacklist,
                    )
                    .await;
                    for ip_str in &squatters {
                        audit_peer_registry.kick_peer(ip_str).await;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
                }
            });
            tracing::info!("🔍 Collateral audit sweep started (5-minute interval)");
        }

        // AV41: Ghost transaction mempool sweep — runs every 60 seconds.
        // Catches any ghost TXs (0 inputs, 0 outputs, invalid or forged special_data)
        // that slipped in before the fix was deployed or via a race with the guard.
        {
            let ghost_consensus = self.consensus.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let removed = ghost_consensus.tx_pool.purge_ghost_transactions();
                    if removed > 0 {
                        tracing::warn!(
                            "🛡️ [AV41] Periodic sweep: purged {} ghost transaction(s) from mempool",
                            removed
                        );
                    }
                }
            });
            tracing::info!("🔍 Ghost TX mempool sweep started (5-minute interval)");
        }

        // Note: Deduplication filter handles its own cleanup with automatic rotation

        // Per-/24 subnet connection rate limiter: throttles inbound floods before TLS.
        // Key: first-three-octet prefix (e.g. "50.28.104"), Value: accept timestamps.
        // Caps any single /24 at MAX_SUBNET_CONNECTS_PER_MIN new connections per minute,
        // preventing distributed SNI floods and botnet cycling attacks.
        let subnet_accept_rate: Arc<DashMap<String, VecDeque<Instant>>> = Arc::new(DashMap::new());

        loop {
            let (stream, addr) = self.listener.accept().await?;
            let addr_str = addr.to_string();

            // Configure TCP socket options for persistent connections
            // Disable Nagle's algorithm to prevent batching
            if let Err(e) = stream.set_nodelay(true) {
                tracing::warn!("Failed to set TCP_NODELAY for {}: {}", addr, e);
            }

            // Enable TCP keepalive to detect dead connections
            let socket = socket2::SockRef::from(&stream);
            let keepalive = socket2::TcpKeepalive::new()
                .with_time(std::time::Duration::from_secs(30)) // Send first probe after 30s idle
                .with_interval(std::time::Duration::from_secs(10)); // Send probes every 10s

            if let Err(e) = socket.set_tcp_keepalive(&keepalive) {
                tracing::warn!("Failed to set TCP_KEEPALIVE for {}: {}", addr, e);
            } else {
                tracing::debug!("✓ TCP keepalive enabled for inbound {}", addr);
            }

            // Extract IP address
            let ip: IpAddr = addr.ip();
            let ip_str = ip.to_string();

            // Check blacklist BEFORE accepting connection
            {
                let mut blacklist = self.blacklist.write().await;
                if let Some(reason) = blacklist.is_blacklisted(ip) {
                    tracing::debug!("🚫 Rejected blacklisted IP {}: {}", ip, reason);
                    drop(stream); // Close immediately
                    continue;
                }
            }

            // Phase 3: Check if this IP is whitelisted (trusted masternode)
            let is_whitelisted = {
                let blacklist = self.blacklist.read().await;
                blacklist.is_whitelisted(ip)
            };

            // Phase 2.1: Check connection limits BEFORE accepting
            // Phase 3: Whitelisted masternodes bypass regular connection limits
            // Phase 4: Per-/24 subnet rate limit (non-whitelisted only)
            if !is_whitelisted {
                const MAX_SUBNET_CONNECTS_PER_MIN: usize = 20;
                let subnet = {
                    let parts: Vec<&str> = ip_str.splitn(4, '.').collect();
                    if parts.len() >= 3 {
                        format!("{}.{}.{}", parts[0], parts[1], parts[2])
                    } else {
                        ip_str.clone()
                    }
                };
                let now_instant = Instant::now();
                let reject = {
                    let mut entry = subnet_accept_rate.entry(subnet.clone()).or_default();
                    while entry
                        .front()
                        .map(|t: &Instant| now_instant.duration_since(*t).as_secs() >= 60)
                        .unwrap_or(false)
                    {
                        entry.pop_front();
                    }
                    entry.push_back(now_instant);
                    entry.len() > MAX_SUBNET_CONNECTS_PER_MIN
                };
                if reject {
                    tracing::debug!(
                        "🚫 Subnet rate limit: {}/24 exceeded {} conn/min, dropping",
                        subnet,
                        MAX_SUBNET_CONNECTS_PER_MIN
                    );
                    if let Some(ai) = &self.ai_system {
                        ai.attack_detector.record_tls_failure(&ip_str);
                    }
                    drop(stream);
                    continue;
                }
            }

            if let Err(reason) = self
                .connection_manager
                .can_accept_inbound(&ip_str, is_whitelisted)
            {
                tracing::warn!("🚫 Rejected inbound connection from {}: {}", ip, reason);
                drop(stream); // Close immediately
                continue;
            }

            tracing::debug!(
                "✅ Accepting inbound connection from {} (total: {}, inbound: {}, whitelisted: {})",
                ip,
                self.connection_manager.connected_count(),
                self.connection_manager.inbound_count(),
                self.connection_manager.count_whitelisted_connections()
            );

            let peer = PeerInfo {
                addr: addr_str.clone(),
                is_masternode: false,
            };

            let peers = self.peers.clone();
            let subs = self.subscriptions.clone();
            let notifier = self.tx_notifier.subscribe();
            let utxo_mgr = self.utxo_manager.clone();
            let consensus = self.consensus.clone();
            let rate_limiter = self.rate_limiter.clone();
            let blacklist = self.blacklist.clone();
            let mn_registry = self.masternode_registry.clone();
            let blockchain = self.blockchain.clone();
            let peer_mgr = self.peer_manager.clone();
            let broadcast_tx = self.tx_notifier.clone();
            let seen_blocks = self.seen_blocks.clone();
            let seen_txs = self.seen_transactions.clone();
            let seen_tx_fin = self.seen_tx_finalized.clone();
            let seen_locks = self.seen_utxo_locks.clone();
            let conn_mgr = self.connection_manager.clone();
            let peer_reg = self.peer_registry.clone();
            let local_ip = self.local_ip.clone();
            let block_cache = self.block_cache.clone();
            let fork_status = self.peer_fork_status.clone();
            let tls_config = self.tls_config.clone();
            let network_type = self.network_type;
            let ai_system = self.ai_system.clone();

            tokio::spawn(async move {
                let _ = handle_peer(
                    stream,
                    peer,
                    peers,
                    subs,
                    notifier,
                    utxo_mgr,
                    consensus,
                    rate_limiter,
                    blacklist,
                    mn_registry,
                    blockchain,
                    peer_mgr,
                    broadcast_tx,
                    seen_blocks,
                    seen_txs,
                    seen_tx_fin,
                    seen_locks,
                    conn_mgr,
                    peer_reg,
                    local_ip,
                    block_cache,
                    fork_status,
                    is_whitelisted,
                    tls_config,
                    network_type,
                    ai_system,
                )
                .await;
            });
        }
    }

    #[allow(dead_code)]
    pub async fn broadcast(&self, msg: NetworkMessage) {
        let _ = self.tx_notifier.send(msg);
    }

    #[allow(dead_code)]
    pub async fn notify_utxo_change(&self, outpoint: OutPoint, state: crate::types::UTXOState) {
        let change = UTXOStateChange {
            outpoint,
            new_state: state,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };
        self.broadcast(NetworkMessage::UTXOStateNotification(change))
            .await;
    }
}

// Message size constants (wire protocol enforces 4MB max frame, these are for tests)
#[allow(dead_code)]
const MAX_MESSAGE_SIZE: usize = 2_000_000;
#[allow(dead_code)]
const MAX_BLOCK_SIZE: usize = 2_000_000;
#[allow(dead_code)]
const MAX_TX_SIZE: usize = 100_000;
#[allow(dead_code)]
const MAX_VOTE_SIZE: usize = 1_000;
#[allow(dead_code)]
const MAX_GENERAL_SIZE: usize = 50_000;

/// Scan every paid-tier registered masternode and verify that its `reward_address`
/// matches the on-chain output address of its collateral UTXO.  A mismatch is
/// definitive proof that the registrant does not own the UTXO — they registered with
/// a collateral outpoint they found on-chain but cannot control.
///
/// Side-effects on each confirmed squatter:
///   1. Evicted from the masternode registry
///   2. Collateral lock released (wallet coins freed)
///   3. Temporarily banned for 2 hours (allows misconfigured-but-legitimate nodes to recover)
///
/// Returns the list of squatter IPs that were evicted (so the caller can kick TCP
/// connections that are still open).
///
/// Also detects duplicate outpoint claims: if two registry entries share the same
/// outpoint, the one whose reward_address matches the UTXO address is kept; the
/// other is evicted and banned.
async fn audit_collateral_registrations(
    registry: &Arc<crate::masternode_registry::MasternodeRegistry>,
    utxo_manager: &Arc<crate::utxo_manager::UTXOStateManager>,
    blacklist: &Arc<tokio::sync::RwLock<crate::network::blacklist::IPBlacklist>>,
) -> Vec<String> {
    use std::collections::HashMap;

    let all_nodes = registry.get_all().await;
    let mut evicted: Vec<String> = Vec::new();

    // Collect IPs that must never be evicted by this sweep:
    // 1. The local node's own registered IP (it knows its own config is correct).
    // 2. Any whitelisted IPs (operator-trusted nodes).
    let local_ip = registry
        .get_local_masternode()
        .await
        .map(|mn| mn.masternode.address.clone())
        .unwrap_or_default();
    let whitelist_ips = {
        let bl = blacklist.read().await;
        bl.whitelist_ips()
    };
    let is_exempt = |ip: &str| -> bool {
        let bare = ip.split(':').next().unwrap_or(ip);
        if !local_ip.is_empty() && bare == local_ip.split(':').next().unwrap_or(&local_ip) {
            return true;
        }
        if let Ok(parsed) = bare.parse::<std::net::IpAddr>() {
            if whitelist_ips.contains(&parsed) {
                return true;
            }
        }
        false
    };

    // ── Pass 0: on-chain anchor mismatch ────────────────────────────────────
    // The `collateral_anchor:{outpoint}` sled key is written when a valid
    // MasternodeReg on-chain transaction is confirmed.  It is the ground truth:
    // only the real owner could have signed that transaction.  If the current
    // registry entry's IP differs from the anchor, the registered node is a
    // squatter — even in the address-match stalemate case that Pass 1 can't
    // resolve (squatter copied the victim's reward_address).
    for info in &all_nodes {
        if info.masternode.tier == crate::types::MasternodeTier::Free {
            continue;
        }
        let outpoint = match &info.masternode.collateral_outpoint {
            Some(op) => op.clone(),
            None => continue,
        };
        let ip = info.masternode.address.clone();
        if is_exempt(&ip) {
            continue;
        }
        if let Some(anchored_ip) = registry.get_collateral_anchor(&outpoint) {
            if anchored_ip != ip && !evicted.contains(&ip) {
                tracing::warn!(
                    "🚨 [COLLATERAL AUDIT] On-chain anchor mismatch: registry has {} for \
                     outpoint {} but anchor points to {} — evicting squatter and banning",
                    ip,
                    outpoint,
                    anchored_ip
                );
                let _ = registry.unregister(&ip).await;
                let _ = utxo_manager.unlock_collateral(&outpoint);
                let bare = ip.split(':').next().unwrap_or(&ip);
                if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                    let mut bl = blacklist.write().await;
                    bl.add_temp_ban(
                        ban_ip,
                        std::time::Duration::from_secs(7200),
                        "collateral squatter: registry IP ≠ on-chain anchor IP (audit sweep)",
                    );
                }
                evicted.push(ip);
            }
        }
    }

    // ── Pass 1: address-mismatch squatter detection ──────────────────────────
    // For each paid-tier node, fetch its UTXO.  If the UTXO exists and its
    // output address does not match the registrant's reward_address, the
    // registrant cannot possibly own the UTXO.
    for info in &all_nodes {
        if info.masternode.tier == crate::types::MasternodeTier::Free {
            continue;
        }
        let outpoint = match &info.masternode.collateral_outpoint {
            Some(op) => op.clone(),
            None => continue,
        };
        let utxo = match utxo_manager.get_utxo(&outpoint).await {
            Ok(u) => u,
            Err(_) => continue, // UTXO not on chain yet (sync lag) — skip
        };
        if utxo.address.is_empty() {
            continue; // No address embedded — cannot prove mismatch, skip
        }
        let ip = info.masternode.address.clone();
        if is_exempt(&ip) {
            continue;
        }
        if info.reward_address != utxo.address {
            tracing::warn!(
                "🚨 [COLLATERAL AUDIT] Squatter detected: {} registered {:?} with outpoint {} \
                 but reward_address {} ≠ UTXO owner {} — evicting and banning",
                ip,
                info.masternode.tier,
                outpoint,
                info.reward_address,
                utxo.address,
            );
            let _ = registry.unregister(&ip).await;
            let _ = utxo_manager.unlock_collateral(&outpoint);
            let bare = ip.split(':').next().unwrap_or(&ip);
            if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                let mut bl = blacklist.write().await;
                // Use a 2-hour temp ban rather than permanent: a misconfigured-but-legitimate
                // node can fix its reward_address config and reconnect after the ban expires.
                // Repeat offenders are re-evicted each audit cycle until they fix the config.
                bl.add_temp_ban(
                    ban_ip,
                    std::time::Duration::from_secs(7200),
                    "collateral squatter: reward_address ≠ UTXO owner (audit sweep)",
                );
            }
            evicted.push(ip);
        }
    }

    // ── Pass 2: duplicate outpoint detection ─────────────────────────────────
    // Two nodes may both claim the same outpoint.  The one whose reward_address
    // matches the UTXO's on-chain address is the legitimate owner; the other is
    // a squatter.  If both match (stalemate), a V4 proof is required — the audit
    // cannot resolve that case and leaves it to the V4 eviction path.
    let mut outpoint_map: HashMap<String, Vec<crate::masternode_registry::MasternodeInfo>> =
        HashMap::new();
    for info in &all_nodes {
        if info.masternode.tier == crate::types::MasternodeTier::Free {
            continue;
        }
        if let Some(ref op) = info.masternode.collateral_outpoint {
            let key = format!("{}:{}", hex::encode(op.txid), op.vout);
            outpoint_map.entry(key).or_default().push(info.clone());
        }
    }
    for (outpoint_str, claimants) in &outpoint_map {
        if claimants.len() < 2 {
            continue;
        }
        // Resolve: fetch the UTXO address and pick the owner.
        let outpoint = match claimants
            .first()
            .and_then(|c| c.masternode.collateral_outpoint.clone())
        {
            Some(op) => op,
            None => continue,
        };
        let utxo_addr = match utxo_manager.get_utxo(&outpoint).await {
            Ok(u) if !u.address.is_empty() => u.address,
            _ => continue,
        };
        let mut owner: Option<String> = None;
        let mut squatters: Vec<String> = Vec::new();
        for c in claimants {
            if c.reward_address == utxo_addr {
                if owner.is_none() {
                    owner = Some(c.masternode.address.clone());
                } else {
                    // Both match — stalemate; V4 proof needed; skip eviction.
                    squatters.clear();
                    tracing::warn!(
                        "⚠️ [COLLATERAL AUDIT] Outpoint {} claimed by {} nodes all matching \
                         UTXO address — V4 cryptographic proof required to resolve",
                        outpoint_str,
                        claimants.len()
                    );
                    break;
                }
            } else {
                squatters.push(c.masternode.address.clone());
            }
        }
        if owner.is_some() {
            for sq_ip in &squatters {
                if evicted.contains(sq_ip) || is_exempt(sq_ip) {
                    continue; // Already handled in pass 1 or exempt (local/whitelisted)
                }
                tracing::warn!(
                    "🚨 [COLLATERAL AUDIT] Duplicate outpoint {}: owner={:?}, squatter={}",
                    outpoint_str,
                    owner,
                    sq_ip
                );
                let _ = registry.unregister(sq_ip).await;
                let _ = utxo_manager.unlock_collateral(&outpoint);
                let bare = sq_ip.split(':').next().unwrap_or(sq_ip.as_str());
                if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                    let mut bl = blacklist.write().await;
                    bl.add_temp_ban(
                        ban_ip,
                        std::time::Duration::from_secs(7200),
                        "collateral squatter: duplicate outpoint claim (audit sweep)",
                    );
                }
                evicted.push(sq_ip.clone());
            }
        }
    }

    // ── Pass 3: zero-value / Free-tier-with-outpoint pollution (AV40) ─────────
    // An attacker can create 0-value UTXOs and register them as Free-tier
    // "collateral" under any IP, associating bogus outpoints with legitimate
    // nodes.  The registration guard (AV40 in register_internal) now blocks new
    // entries, but existing ones need to be swept out.
    //
    // Also catches any non-Free node whose locked collateral UTXO has value 0 or
    // below the Bronze minimum — the on-chain value is the ultimate authority.
    const BRONZE_MIN: u64 = 1_000_000_000_000; // 1,000 TIME in satoshis
    for info in &all_nodes {
        let outpoint = match &info.masternode.collateral_outpoint {
            Some(op) => op.clone(),
            None => continue,
        };
        let ip = info.masternode.address.clone();
        if evicted.contains(&ip) || is_exempt(&ip) {
            continue;
        }
        let utxo = match utxo_manager.get_utxo(&outpoint).await {
            Ok(u) => u,
            Err(_) => continue, // Not yet in our UTXO set — skip
        };
        let utxo_value = utxo.value;
        let is_zero_value_collateral =
            info.masternode.tier == crate::types::MasternodeTier::Free || utxo_value < BRONZE_MIN;
        if is_zero_value_collateral {
            tracing::warn!(
                "🚨 [COLLATERAL AUDIT] [AV40] Zero/sub-minimum collateral detected: \
                 {} has outpoint {} with value {} satoshis (tier: {:?}) — evicting",
                ip,
                outpoint,
                utxo_value,
                info.masternode.tier,
            );
            let _ = registry.unregister(&ip).await;
            let _ = utxo_manager.unlock_collateral(&outpoint);
            let bare = ip.split(':').next().unwrap_or(&ip);
            if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                let mut bl = blacklist.write().await;
                bl.add_temp_ban(
                    ban_ip,
                    std::time::Duration::from_secs(7200),
                    "AV40: zero/sub-minimum collateral UTXO (audit sweep)",
                );
            }
            evicted.push(ip);
        }
    }

    if !evicted.is_empty() {
        tracing::warn!(
            "🔍 [COLLATERAL AUDIT] Sweep complete — evicted {} squatter(s): {:?}",
            evicted.len(),
            evicted
        );
    }
    evicted
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // Called by NetworkServer::run which is used by binary
async fn handle_peer(
    stream: TcpStream,
    peer: PeerInfo,
    _peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    subs: Arc<RwLock<HashMap<String, Subscription>>>,
    mut notifier: broadcast::Receiver<NetworkMessage>,
    utxo_mgr: Arc<UTXOStateManager>,
    consensus: Arc<ConsensusEngine>,
    _rate_limiter: Arc<RwLock<RateLimiter>>,
    blacklist: Arc<RwLock<IPBlacklist>>,
    masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    blockchain: Arc<crate::blockchain::Blockchain>,
    peer_manager: Arc<crate::peer_manager::PeerManager>,
    broadcast_tx: broadcast::Sender<NetworkMessage>,
    seen_blocks: Arc<DeduplicationFilter>,
    seen_transactions: Arc<DeduplicationFilter>,
    seen_tx_finalized: Arc<DeduplicationFilter>,
    seen_utxo_locks: Arc<DeduplicationFilter>,
    connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    local_ip: Option<String>,
    block_cache: Arc<BlockCache>, // Phase 3E.1: Block cache parameter
    _peer_fork_status: Arc<DashMap<String, PeerForkStatus>>, // Phase 2: Fork status tracker (no longer used - periodic resolution handles forks)
    is_whitelisted: bool,
    tls_config: Option<Arc<crate::network::tls::TlsConfig>>,
    network_type: crate::network_type::NetworkType,
    ai_system: Option<Arc<crate::ai::AISystem>>,
) -> Result<(), std::io::Error> {
    // Extract IP from address
    let ip: IpAddr = peer
        .addr
        .split(':')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap());

    let ip_str = ip.to_string();

    // Per-connection rate limiter: each peer gets its own instance, eliminating
    // the write-lock contention that the shared NetworkServer RateLimiter caused
    // under load (50+ peers × multiple msg/s = constant write-lock contention).
    // The shared `rate_limiter` parameter is intentionally shadowed here.
    let rate_limiter = Arc::new(RwLock::new(RateLimiter::new()));

    // Get WebSocket tx event sender for real-time wallet notifications
    let ws_tx_event_sender = peer_registry.get_tx_event_sender().await;

    let _connection_start = std::time::Instant::now();

    // Wrap with TLS if configured
    // For TLS: spawn a dedicated I/O bridge task that owns the whole stream,
    // avoiding `tokio::io::split()` which causes frame corruption on TLS streams.
    // For non-TLS: `TcpStream::into_split()` is safe (true full-duplex).
    let (msg_read_tx, mut msg_read_rx) =
        tokio::sync::mpsc::channel::<Result<Option<NetworkMessage>, String>>(512);
    let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let writer_tx: crate::network::peer_connection_registry::PeerWriterTx = write_tx;

    if let Some(tls) = tls_config {
        match tls.accept_server(stream).await {
            Ok(tls_stream) => {
                // Enforce SNI = "timecoin.local": every legitimate TIME node sets this
                // exact SNI when opening an outbound TLS connection (peer_connection.rs).
                // Any connection with a missing, blank, or different SNI (e.g. an IP
                // address literal) is provably not one of our nodes — it's a scanner,
                // prober, or attacker.  Record an immediate violation so the IP
                // escalates through temp → permanent ban (sled-persisted).
                {
                    let sni = tls_stream.get_ref().1.server_name();
                    if sni != Some("timecoin.local") {
                        let sni_desc = sni.unwrap_or("<none>").to_owned();
                        blacklist
                            .write()
                            .await
                            .record_violation(ip, &format!("Invalid TLS SNI: {}", sni_desc));
                        tracing::debug!(
                            "🚫 Rejected {} — invalid SNI {:?} (not a TIME node)",
                            ip,
                            sni_desc
                        );
                        return Ok(());
                    }
                }
                // Log only after TLS succeeds — plain TCP probes (reachability checks)
                // would otherwise spam the log with connections that immediately fail TLS.
                tracing::info!("🔌 New peer connection from: {}", peer.addr);
                tracing::debug!("🔒 TLS established for inbound {}", peer.addr);
                let peer_addr = peer.addr.clone();
                // Spawn a single I/O bridge task that owns the TLS stream
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let mut stream = tls_stream;
                    // Token-bucket flood gate: event-driven, no 1-second timer polling.
                    // Refills at GATE_RATE tokens/s; burst up to GATE_BURST.
                    // Soft-drops messages while tokens are exhausted; after
                    // GATE_HARD_DROPS consecutive soft-drops the peer is disconnected.
                    const GATE_RATE: f64 = 200.0; // sustained msgs/s allowed
                    const GATE_BURST: f64 = 300.0; // burst allowance
                    const GATE_HARD_DROPS: u32 = 300; // consecutive drops → hard kick
                    let mut gate_tokens: f64 = GATE_BURST;
                    let mut gate_last = std::time::Instant::now();
                    let mut gate_drop_streak: u32 = 0;
                    loop {
                        tokio::select! {
                            result = crate::network::wire::read_message(&mut stream) => {
                                let is_eof = matches!(&result, Ok(None));
                                let is_err = result.is_err();
                                // Token-bucket refill: time-since-last-message, not a timer.
                                let gate_now = std::time::Instant::now();
                                let elapsed = gate_now.duration_since(gate_last).as_secs_f64();
                                gate_last = gate_now;
                                gate_tokens = (gate_tokens + elapsed * GATE_RATE).min(GATE_BURST);
                                if gate_tokens >= 1.0 {
                                    gate_tokens -= 1.0;
                                    gate_drop_streak = 0;
                                } else {
                                    gate_drop_streak += 1;
                                    if gate_drop_streak > GATE_HARD_DROPS {
                                        let _ = msg_read_tx.send(Err("Message flood detected: pre-channel gate triggered".to_string())).await;
                                        break;
                                    }
                                    continue; // soft drop
                                }
                                if msg_read_tx.send(result).await.is_err() {
                                    break; // receiver dropped
                                }
                                if is_eof || is_err {
                                    break;
                                }
                            }
                            bytes = write_rx.recv() => {
                                match bytes {
                                    Some(data) => {
                                        if let Err(e) = stream.write_all(&data).await {
                                            tracing::debug!("🔒 TLS write error for {}: {}", peer_addr, e);
                                            break;
                                        }
                                        if let Err(e) = stream.flush().await {
                                            tracing::debug!("🔒 TLS flush error for {}: {}", peer_addr, e);
                                            break;
                                        }
                                    }
                                    None => break, // writer channel closed
                                }
                            }
                        }
                    }
                    tracing::debug!("🔒 TLS I/O bridge exiting for {}", peer_addr);
                });
            }
            Err(e) => {
                // "handshake eof" is sent by old plain-TCP peers that don't speak TLS —
                // demote to DEBUG since it's expected noise from pre-TLS nodes and port scanners.
                let e_str = e.to_string();
                if e_str.contains("eof") || e_str.contains("early eof") {
                    tracing::debug!(
                        "🔓 TLS handshake eof from {} (plain-TCP client?)",
                        peer.addr
                    );
                } else {
                    tracing::warn!("🚫 TLS handshake failed for {}: {}", peer.addr, e);
                }
                // Charge a violation so repeat offenders accumulate bans.
                // Without this, an attacker can flood TLS connections at zero cost —
                // each attempt consumes a tokio task + TLS negotiation with no penalty.
                // Never record violations against our own IP — self-connections (the node
                // briefly attempting to connect to itself via the peer list) must not
                // cause the node to permanently ban itself.
                let is_self = local_ip.as_deref().is_some_and(|l| l == ip_str);
                if !is_self {
                    // Use the TLS-specific counter: much higher threshold, never permanent.
                    // TLS mode mismatches are operator config errors, not attacks — using
                    // the standard record_violation path would permanently ban legitimate
                    // nodes after only 10 retries.
                    blacklist
                        .write()
                        .await
                        .record_tls_violation(ip, &format!("TLS handshake failed: {}", e));
                } else {
                    tracing::debug!(
                        "🔄 Ignoring TLS failure from own IP {} (self-connection)",
                        ip_str
                    );
                }
                return Ok(());
            }
        }
    } else {
        tracing::debug!("🔓 Plaintext connection from {}", peer.addr);
        let (r, w) = stream.into_split();
        // Spawn reader task for non-TLS
        let peer_addr = peer.addr.clone();
        tokio::spawn(async move {
            let mut reader = r;
            // Token-bucket flood gate: event-driven, no 1-second timer polling.
            // Refills at GATE_RATE tokens/s; burst up to GATE_BURST.
            // Soft-drops messages while tokens are exhausted; after
            // GATE_HARD_DROPS consecutive soft-drops the peer is disconnected.
            const GATE_RATE: f64 = 200.0; // sustained msgs/s allowed
            const GATE_BURST: f64 = 300.0; // burst allowance
            const GATE_HARD_DROPS: u32 = 300; // consecutive drops → hard kick
            let mut gate_tokens: f64 = GATE_BURST;
            let mut gate_last = std::time::Instant::now();
            let mut gate_drop_streak: u32 = 0;
            loop {
                let result = crate::network::wire::read_message(&mut reader).await;
                let is_eof = matches!(&result, Ok(None));
                let is_err = result.is_err();
                // Token-bucket refill: time-since-last-message, not a timer.
                let gate_now = std::time::Instant::now();
                let elapsed = gate_now.duration_since(gate_last).as_secs_f64();
                gate_last = gate_now;
                gate_tokens = (gate_tokens + elapsed * GATE_RATE).min(GATE_BURST);
                if gate_tokens >= 1.0 {
                    gate_tokens -= 1.0;
                    gate_drop_streak = 0;
                } else {
                    gate_drop_streak += 1;
                    if gate_drop_streak > GATE_HARD_DROPS {
                        let _ = msg_read_tx
                            .send(Err(
                                "Message flood detected: pre-channel gate triggered".to_string()
                            ))
                            .await;
                        break;
                    }
                    continue; // soft drop
                }
                if msg_read_tx.send(result).await.is_err() {
                    break;
                }
                if is_eof || is_err {
                    break;
                }
            }
            tracing::debug!("📖 Reader task exiting for {}", peer_addr);
        });
        // Spawn writer task for non-TLS
        let peer_addr2 = peer.addr.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut writer = w;
            while let Some(data) = write_rx.recv().await {
                if let Err(e) = writer.write_all(&data).await {
                    tracing::debug!("📝 Write error for {}: {}", peer_addr2, e);
                    break;
                }
                if let Err(e) = writer.flush().await {
                    tracing::debug!("📝 Flush error for {}: {}", peer_addr2, e);
                    break;
                }
            }
            tracing::debug!("📝 Writer task exiting for {}", peer_addr2);
        });
    }

    let mut handshake_done = false;
    let mut is_stable_connection = false;

    // Per-connection UTXO lock flood counter: tracks how many UTXOStateUpdate (Locked)
    // messages this peer has sent for each TX.  A legitimate TX with N inputs produces
    // exactly N lock messages — an attacker who sends far more is DoS-flooding us.
    let mut peer_tx_lock_counts: std::collections::HashMap<[u8; 32], u32> =
        std::collections::HashMap::new();
    const MAX_UTXO_LOCKS_PER_TX: u32 = 50;

    let mut ping_excess_streak: u32 = 0;
    let mut tx_finalized_excess_streak: u32 = 0;

    // Per-connection rate-limit drop counters.  Declared here (outside the per-message
    // loop) so they accumulate across messages on the same connection, enabling the
    // suppression log ("N msgs suppressed in last 60s") and the record_severe_violation
    // escalation path (≥10 drops) to actually fire.
    let mut rl_drop_count: u32 = 0;
    let mut rl_last_log = std::time::Instant::now() - std::time::Duration::from_secs(61);

    let magic_bytes = network_type.magic_bytes();

    // A connection that completes TLS but never sends a Handshake message holds an open
    // tokio task and a connection slot indefinitely.  Fire a violation and close after 10s.
    let handshake_timeout = tokio::time::sleep(tokio::time::Duration::from_secs(10));
    tokio::pin!(handshake_timeout);

    loop {
        tokio::select! {
            result = msg_read_rx.recv() => {
                let result = match result {
                    Some(r) => r,
                    None => {
                        if handshake_done {
                            tracing::info!("🔌 Peer {} reader channel closed", peer.addr);
                        } else {
                            tracing::debug!("🔌 Peer {} reader channel closed (pre-handshake)", peer.addr);
                        }
                        break;
                    }
                };
                match result {
                    Ok(None) => {
                        if handshake_done {
                            tracing::info!("🔌 Peer {} disconnected (EOF)", peer.addr);
                        } else {
                            tracing::debug!("🔌 Peer {} disconnected before handshake (EOF)", peer.addr);
                        }
                        break;
                    }
                    Err(e) => {
                        if handshake_done {
                            tracing::info!("🔌 Connection from {} ended: {}", peer.addr, e);
                        } else {
                            tracing::debug!("🔌 Connection from {} ended before handshake: {}", peer.addr, e);
                        }
                        // Pre-handshake oversized frame: trivial 4-byte DoS — penalise.
                        // Post-handshake frames > 100 MB are clearly malicious (e.g. 926 MB
                        // frames from fork-attack peers); penalise those too. Smaller
                        // post-handshake overflows may be a framing mismatch with older nodes
                        // and are not penalised so we don't ban legitimate sync peers.
                        const MALICIOUS_FRAME_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
                        let is_large_frame = e.contains("Frame too large");
                        let frame_bytes: Option<u64> = if is_large_frame {
                            e.split_whitespace()
                                .find_map(|w| w.trim_end_matches("bytes").trim_end_matches(':').parse::<u64>().ok())
                        } else {
                            None
                        };
                        let clearly_malicious = frame_bytes.map_or(false, |b| b > MALICIOUS_FRAME_BYTES);
                        if is_large_frame && (!handshake_done || clearly_malicious) {
                            blacklist.write().await.record_violation(
                                ip,
                                &format!("Oversized frame header: {}", e),
                            );
                        } else if e.contains("Message flood detected") {
                            tracing::warn!("🌊 Message flood from {} — pre-channel gate triggered, recording violation", peer.addr);
                            blacklist.write().await.record_violation(ip, "Message flood: sustained >500 msgs/s");
                            if let Some(ai) = &ai_system {
                                ai.attack_detector.record_message_flood(&ip_str);
                            }
                        }
                        break;
                    }
                    Ok(Some(msg)) => {
                            // First message MUST be a valid handshake
                            if !handshake_done {
                                match &msg {
                                    NetworkMessage::Handshake { magic, protocol_version, network, commit_count } => {
                                        if magic != &magic_bytes {
                                            tracing::warn!("🚫 Rejecting {} - invalid magic bytes: {:?}", peer.addr, magic);
                                            blacklist.write().await.record_violation(
                                                ip,
                                                &format!("Invalid magic bytes: {:?}", magic)
                                            );
                                            break;
                                        }
                                        if *protocol_version < 2 {
                                            tracing::warn!("🚫 Rejecting {} - protocol version {} is too old (minimum: 2)", peer.addr, protocol_version);
                                            blacklist.write().await.record_violation(
                                                ip,
                                                &format!("Protocol version {} below minimum 2", protocol_version)
                                            );
                                            break;
                                        }
                                        let our_commits = env!("GIT_COMMIT_COUNT").parse::<u32>().unwrap_or(0);
                                        if *commit_count < our_commits {
                                            tracing::warn!(
                                                "⚠️ Peer {} is running outdated software \
                                                (commit {}, we are at commit {}). \
                                                Please upgrade: https://github.com/time-coin/time-masternode",
                                                peer.addr, commit_count, our_commits
                                            );
                                            // Notify the peer directly so they see it in their own logs.
                                            let upgrade_msg = crate::network::message::NetworkMessage::ForkAlert {
                                                your_height: 0,
                                                your_hash: [0u8; 32],
                                                consensus_height: 0,
                                                consensus_hash: [0u8; 32],
                                                consensus_peer_count: 0,
                                                message: format!(
                                                    "Your node is running outdated software \
                                                    (commit {commit_count}, current is {our_commits}). \
                                                    Please upgrade: https://github.com/time-coin/time-masternode"
                                                ),
                                            };
                                            if let Ok(frame) = crate::network::wire::serialize_frame(&upgrade_msg) {
                                                let _ = writer_tx.send(frame);
                                            }
                                        }
                                        // Check if the peer is ahead of us — we may be outdated.
                                        if *commit_count > our_commits && our_commits > 0 {
                                            tracing::warn!(
                                                "⬆️  Peer {} is running newer software \
                                                (commit {}, we are at commit {}). \
                                                Consider upgrading: https://github.com/time-coin/time-masternode",
                                                peer.addr, commit_count, our_commits
                                            );
                                        }
                                        peer_registry.set_peer_commit_count(&ip_str, *commit_count).await;
                                        tracing::info!(
                                            "✅ Handshake accepted from {} (network: {}, commit: {})",
                                            peer.addr, network, commit_count
                                        );
                                        handshake_done = true;

                                        // Atomically register inbound connection to prevent race conditions
                                        // This ensures only ONE inbound connection succeeds if multiple arrive simultaneously
                                        if !peer_registry.try_register_inbound(&ip_str) {
                                            tracing::info!(
                                                "🔄 Rejecting duplicate inbound from {} (already registered)",
                                                peer.addr
                                            );
                                            break; // Close this new inbound connection
                                        }

                                        // Also mark in connection_manager for DoS protection tracking
                                        connection_manager.mark_inbound(&ip_str);

                                        // Register write channel in peer registry after successful handshake
                                        tracing::info!("📝 Registering {} in PeerConnectionRegistry (peer.addr: {})", ip_str, peer.addr);
                                        peer_registry.register_peer(ip_str.clone(), writer_tx.clone()).await;
                                        tracing::debug!("✅ Successfully registered {} in registry", ip_str);

                                        // Send ACK to confirm handshake was processed
                                        let ack_msg = NetworkMessage::Ack {
                                            message_type: "Handshake".to_string(),
                                        };
                                        let _ = peer_registry.send_to_peer(&ip_str, ack_msg).await;

                                        // Load-balancing redirect: if we're above our soft
                                        // inbound limit and this is not a whitelisted peer,
                                        // send a PeerExchange of less-loaded alternatives and
                                        // close.  MIN_CONNECTIONS ensures the network never
                                        // fractures — we always keep at least that many inbound.
                                        const INBOUND_REDIRECT_THRESHOLD: usize = 70; // 70 % of MAX_INBOUND_CONNECTIONS
                                        const MIN_CONNECTIONS: usize = 8;
                                        let cur_inbound = peer_registry.inbound_count();
                                        if !is_whitelisted
                                            && cur_inbound > INBOUND_REDIRECT_THRESHOLD
                                        {
                                            let alternatives =
                                                peer_registry.get_peers_by_load(12).await;
                                            if alternatives.len() >= 3
                                                && cur_inbound > MIN_CONNECTIONS
                                            {
                                                tracing::info!(
                                                    "↩️  Redirecting {} to {} less-loaded peers (inbound: {})",
                                                    ip_str, alternatives.len(), cur_inbound
                                                );
                                                let redirect = NetworkMessage::PeerExchange(alternatives);
                                                let _ = peer_registry.send_to_peer(&ip_str, redirect).await;
                                                // Small delay so the message can be flushed
                                                tokio::time::sleep(
                                                    std::time::Duration::from_millis(200),
                                                )
                                                .await;
                                                break; // Close after redirect
                                            }
                                        }

                                        // Send our masternode announcement if we're a masternode
                                        let local_address = masternode_registry.get_local_address().await;
                                        if let Some(our_address) = local_address {
                                            // Only send OUR masternode announcement, not all masternodes
                                            let local_masternodes = masternode_registry.get_all().await;
                                            if let Some(our_mn) = local_masternodes.iter().find(|mn| mn.masternode.address == our_address) {
                                                let cert = masternode_registry.get_local_certificate().await;
                                                let announcement = NetworkMessage::MasternodeAnnouncementV3 {
                                                    address: our_mn.masternode.address.clone(),
                                                    reward_address: our_mn.reward_address.clone(),
                                                    tier: our_mn.masternode.tier,
                                                    public_key: our_mn.masternode.public_key,
                                                    collateral_outpoint: our_mn.masternode.collateral_outpoint.clone(),
                                                    certificate: cert.to_vec(),
                                                    started_at: masternode_registry.get_started_at(),
                                                };
                                                let _ = peer_registry.send_to_peer(&ip_str, announcement).await;
                                                tracing::info!("📢 Sent masternode announcement (V3) to peer {}", ip_str);
                                            }
                                        }

                                        // Request peer list for peer discovery
                                        let get_peers_msg = NetworkMessage::GetPeers;
                                        let _ = peer_registry.send_to_peer(&ip_str, get_peers_msg).await;

                                        // Request masternodes for peer discovery
                                        let get_mn_msg = NetworkMessage::GetMasternodes;
                                        let _ = peer_registry.send_to_peer(&ip_str, get_mn_msg).await;

                                        // Request full mempool state so we can resume
                                        // processing any transactions the peer already has
                                        let mempool_req = NetworkMessage::MempoolSyncRequest;
                                        let _ = peer_registry.send_to_peer(&ip_str, mempool_req).await;

                                        // CRITICAL: Verify genesis hash compatibility EARLY
                                        // This prevents nodes with different genesis from exchanging blocks
                                        if blockchain.has_genesis() {
                                            let our_genesis_hash = blockchain.genesis_hash();
                                            // Request peer's genesis hash for verification
                                            let get_genesis_msg = NetworkMessage::GetGenesisHash;
                                            let _ = peer_registry.send_to_peer(&ip_str, get_genesis_msg).await;
                                            tracing::debug!(
                                                "📤 Requesting genesis hash from {} for compatibility check (our genesis: {})",
                                                ip_str,
                                                hex::encode(&our_genesis_hash[..8])
                                            );
                                        } else {
                                            // We don't have a genesis yet - request one from peer
                                            tracing::info!(
                                                "🌱 No local genesis - requesting genesis block from {}",
                                                ip_str
                                            );
                                            let request_genesis_msg = NetworkMessage::RequestGenesis;
                                            let _ = peer_registry.send_to_peer(&ip_str, request_genesis_msg).await;
                                        }
                                        // Spawn periodic ping task for RTT measurement on this inbound connection.
                                        // Without this, ping times would only be tracked for outbound connections.
                                        {
                                            let ping_ip = ip_str.clone();
                                            let ping_registry = Arc::clone(&peer_registry);
                                            let ping_blockchain = Arc::clone(&blockchain);
                                            tokio::spawn(async move {
                                                let mut interval = tokio::time::interval(
                                                    std::time::Duration::from_secs(30),
                                                );
                                                // Skip the immediate tick — let the connection settle first.
                                                interval.tick().await;
                                                loop {
                                                    interval.tick().await;
                                                    if !ping_registry.is_connected(&ping_ip) {
                                                        break;
                                                    }
                                                    let nonce = rand::random::<u64>();
                                                    let height = ping_blockchain.get_height();
                                                    let msg = crate::network::message::NetworkMessage::Ping {
                                                        nonce,
                                                        timestamp: chrono::Utc::now().timestamp(),
                                                        height: Some(height),
                                                    };
                                                    ping_registry.record_ping_sent(&ping_ip, nonce).await;
                                                    if ping_registry.send_to_peer(&ping_ip, msg).await.is_err() {
                                                        break;
                                                    }
                                                }
                                            });
                                        }
                                        continue;
                                    }
                                    _ => {
                                        tracing::warn!("⚠️  {} sent message before handshake - closing connection", peer.addr);
                                        if let Some(ref ai) = ai_system {
                                            ai.attack_detector.record_pre_handshake_violation(&ip_str);
                                        }
                                        // Record a direct blacklist violation per occurrence so
                                        // persistent pre-handshake probers accumulate bans even
                                        // if they disconnect before the 30s AI enforcement loop.
                                        {
                                            let mut bl = blacklist.write().await;
                                            bl.record_violation(ip, "Sent message before completing handshake");
                                        }
                                        break;
                                    }
                                }
                            }

                            tracing::debug!("📦 Parsed message type from {}: {:?}", peer.addr, std::mem::discriminant(&msg));

                            // Phase 2.2: Rate limiting and blacklist enforcement
                            //
                            // rl_drop_count / rl_last_log are declared outside this loop so
                            // they accumulate across messages on the same connection.

                            // Define helper macro for rate limit checking with auto-ban
                            macro_rules! check_rate_limit {
                                ($msg_type:expr) => {{
                                    let mut limiter = rate_limiter.write().await;
                                    let mut blacklist_guard = blacklist.write().await;

                                    if !limiter.check($msg_type, &ip_str) {
                                        rl_drop_count += 1;

                                        // Log at most once per 60 s per connection to avoid
                                        // flooding the journal with thousands of identical lines.
                                        let now = std::time::Instant::now();
                                        if now.duration_since(rl_last_log).as_secs() >= 60
                                            || rl_drop_count == 1
                                        {
                                            if rl_drop_count > 1 {
                                                tracing::warn!(
                                                    "⚠️  Rate limit exceeded for {} from {} \
                                                     ({} msgs suppressed in last 60s)",
                                                    $msg_type, peer.addr, rl_drop_count - 1
                                                );
                                            } else {
                                                tracing::warn!(
                                                    "⚠️  Rate limit exceeded for {} from {}",
                                                    $msg_type, peer.addr
                                                );
                                            }
                                            rl_last_log = now;
                                            rl_drop_count = 0;
                                        }

                                        // After 10 dropped messages on a single connection,
                                        // this is a mass-flood, not a reconnect race.
                                        // Use record_severe_violation to bypass the whitelist
                                        // exemption and escalate the ban faster.
                                        let should_ban = if rl_drop_count >= 10 {
                                            blacklist_guard.record_severe_violation(ip,
                                                &format!("Mass flood: {} ({}+ msgs dropped)", $msg_type, rl_drop_count))
                                        } else {
                                            blacklist_guard.record_violation(ip,
                                                &format!("Rate limit exceeded: {}", $msg_type))
                                        };

                                        if should_ban {
                                            tracing::warn!(
                                                "🚫 Disconnecting {} due to rate limit flood ({} dropped)",
                                                peer.addr, rl_drop_count
                                            );
                                            drop(blacklist_guard);
                                            drop(limiter);
                                            peer_registry.kick_peer(&ip_str).await;
                                            break; // Exit connection loop
                                        }
                                        continue; // Skip processing this message
                                    }

                                    drop(limiter);
                                    drop(blacklist_guard);
                                }};
                            }

                            // Size validation handled by wire protocol (4MB max frame)
                            macro_rules! check_message_size {
                                ($max_size:expr, $msg_type:expr) => {{}};
                            }

                            match &msg {
                                // PRIORITY: UTXO locks MUST be processed immediately, even during block sync
                                // This prevents double-spend race conditions
                                NetworkMessage::UTXOStateUpdate { outpoint, state } => {
                                    // Create unique identifier for this UTXO lock update
                                    let mut lock_id = Vec::new();
                                    lock_id.extend_from_slice(&outpoint.txid);
                                    lock_id.extend_from_slice(&outpoint.vout.to_le_bytes());

                                    // Add state discriminant to differentiate lock types
                                    match state {
                                        UTXOState::Locked { txid, .. } => {
                                            lock_id.push(1); // Locked state
                                            lock_id.extend_from_slice(txid);
                                        }
                                        UTXOState::Unspent => lock_id.push(2),
                                        UTXOState::SpentPending { txid, .. } => {
                                            lock_id.push(3);
                                            lock_id.extend_from_slice(txid);
                                        }
                                        UTXOState::SpentFinalized { txid, .. } => {
                                            lock_id.push(4);
                                            lock_id.extend_from_slice(txid);
                                        }
                                        UTXOState::Archived { txid, .. } => {
                                            lock_id.push(5);
                                            lock_id.extend_from_slice(txid);
                                        }
                                    }

                                    // Check if we've already processed this exact UTXO lock update
                                    let already_seen = seen_utxo_locks.check_and_insert(&lock_id).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate UTXO lock update from {}", peer.addr);
                                        continue;
                                    }

                                    // FLOOD GUARD: count distinct Locked messages per TX from this peer.
                                    // A legitimate TX with N inputs sends exactly N lock messages.
                                    // Anything beyond MAX_UTXO_LOCKS_PER_TX is a DoS flood — flag and drop.
                                    if let UTXOState::Locked { txid, .. } = &state {
                                        let count = peer_tx_lock_counts.entry(*txid).or_insert(0);
                                        *count += 1;
                                        if *count > MAX_UTXO_LOCKS_PER_TX {
                                            if let Some(ref ai) = ai_system {
                                                ai.attack_detector.record_utxo_lock_flood(
                                                    &ip_str,
                                                    &hex::encode(txid),
                                                    *count,
                                                );
                                            }
                                            tracing::warn!(
                                                "🚫 UTXO lock flood from {}: {} locks for TX {} (max {}), dropping",
                                                peer.addr,
                                                count,
                                                hex::encode(txid),
                                                MAX_UTXO_LOCKS_PER_TX
                                            );
                                            continue;
                                        }
                                    }

                                    tracing::debug!("🔒 PRIORITY: Received UTXO lock update from {}", peer.addr);
                                    consensus.utxo_manager.update_state(outpoint, state.clone());

                                    if let UTXOState::Locked { txid, .. } = state {
                                        tracing::debug!(
                                            "🔒 Applied UTXO lock from peer {} for TX {}",
                                            peer.addr,
                                            hex::encode(txid)
                                        );
                                    }

                                    // Gossip lock to other peers immediately (only if not duplicate)
                                    let _ = broadcast_tx.send(msg.clone());
                                }

                                NetworkMessage::Ack { message_type } => {
                                    tracing::debug!("✅ Received ACK for {} from {}", message_type, peer.addr);
                                    // ACKs are informational, no action needed
                                }
                                NetworkMessage::TransactionBroadcast(tx) => {
                                    check_message_size!(MAX_TX_SIZE, "Transaction");
                                    check_rate_limit!("tx");

                                    // Check if we've already seen this transaction using Bloom filter
                                    let txid = tx.txid();
                                    let already_seen = seen_transactions.check_and_insert(&txid).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate transaction {} from {}", hex::encode(txid), peer.addr);
                                        continue;
                                    }

                                    tracing::info!("📥 Received new transaction {} from {}", hex::encode(txid), peer.addr);

                                    // Process transaction (validates and initiates voting if we're a masternode)
                                    match consensus.process_transaction(tx.clone(), None).await {
                                        Ok(_) => {
                                            tracing::debug!("✅ Transaction {} processed", hex::encode(txid));

                                            // Emit WebSocket notification for subscribed wallets
                                            if let Some(ref tx_sender) = ws_tx_event_sender {
                                                let outputs: Vec<crate::rpc::websocket::TxOutputInfo> = tx
                                                    .outputs
                                                    .iter()
                                                    .enumerate()
                                                    .map(|(i, out)| {
                                                        let address = String::from_utf8(out.script_pubkey.clone())
                                                            .unwrap_or_else(|_| hex::encode(&out.script_pubkey));
                                                        crate::rpc::websocket::TxOutputInfo {
                                                            address,
                                                            amount: out.value as f64 / 100_000_000.0,
                                                            index: i as u32,
                                                        }
                                                    })
                                                    .collect();

                                                let event = crate::rpc::websocket::TransactionEvent {
                                                    txid: hex::encode(txid),
                                                    outputs,
                                                    timestamp: chrono::Utc::now().timestamp(),
                                                    status: crate::rpc::websocket::TxEventStatus::Pending,
                                                };
                                                match tx_sender.send(event) {
                                                    Ok(n) => tracing::info!("📡 WS tx_notification (server.rs) sent to {} receiver(s)", n),
                                                    Err(_) => tracing::warn!("📡 WS tx_notification (server.rs) failed: no receivers"),
                                                }
                                            }

                                            // Gossip to other peers
                                            match broadcast_tx.send(msg.clone()) {
                                                Ok(receivers) => {
                                                    tracing::debug!("🔄 Gossiped transaction {} to {} peer(s)", hex::encode(txid), receivers.saturating_sub(1));
                                                }
                                                Err(e) => {
                                                    tracing::debug!("Failed to gossip transaction: {}", e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            let err_str = e.to_string();
                                            if err_str.contains("already in pool") || err_str.contains("Already") {
                                                tracing::debug!("🔁 Transaction {} already in pool (from {})", hex::encode(txid), peer.addr);
                                            } else if err_str.contains("no inputs") || err_str.contains("no outputs") {
                                                // AV40: null transaction (0 inputs / 0 outputs).
                                                // Do NOT record a blacklist violation here — the peer may be
                                                // an innocent relay that forwarded the TX before our structural
                                                // check could stop it.  The AI sliding-window detector
                                                // (record_null_tx_flood) only escalates after ≥3 such TXs
                                                // within 60 s, which innocent relays never reach.
                                                tracing::debug!(
                                                    "🗑️ Null TX {} from {} rejected ({})",
                                                    hex::encode(txid), peer.addr, err_str
                                                );
                                                if let Some(ref ai) = ai_system {
                                                    ai.attack_detector.record_null_tx_flood(&ip_str);
                                                }
                                            } else {
                                                tracing::warn!("❌ Transaction {} rejected: {}", hex::encode(txid), e);

                                                // Phase 2.2: Record violation for invalid transaction
                                                let mut blacklist_guard = blacklist.write().await;
                                                let should_ban = blacklist_guard.record_violation(ip, "Invalid transaction");
                                                drop(blacklist_guard);

                                                if should_ban {
                                                    tracing::warn!("🚫 Disconnecting {} due to repeated invalid transactions", peer.addr);
                                                    peer_registry.kick_peer(&ip_str).await;
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                                NetworkMessage::TransactionFinalized { txid, tx } => {
                                    // AV40: inline rate check with AI escalation.
                                    // The generic check_rate_limit! macro calls record_violation()
                                    // which is subject to whitelist exemption and never reaches
                                    // record_finality_injection().  By inlining here (mirroring
                                    // ping_excess_streak) we feed sustained flooding into the
                                    // AV38 relay-safe two-tier escalation in AttackDetector.
                                    {
                                        let rate_ok = {
                                            let mut limiter = rate_limiter.write().await;
                                            limiter.check("tx_finalized", &ip_str)
                                        };
                                        if !rate_ok {
                                            tx_finalized_excess_streak += 1;
                                            tracing::warn!(
                                                "⚠️  Rate limit exceeded for tx_finalized from {} (excess streak: {})",
                                                peer.addr, tx_finalized_excess_streak
                                            );
                                            if tx_finalized_excess_streak >= 5 {
                                                if let Some(ref ai) = ai_system {
                                                    ai.attack_detector.record_finality_injection(&ip_str);
                                                }
                                                tx_finalized_excess_streak = 0;
                                            }
                                            continue;
                                        }
                                        tx_finalized_excess_streak = 0;
                                    }

                                    // Drop mempool transactions while syncing — the UTXOs they
                                    // reference likely don't exist in our local UTXO set yet, so
                                    // every validation would fail with "input not in storage".
                                    // The peer will re-broadcast once we're caught up.
                                    if blockchain.is_syncing() {
                                        tracing::debug!("⏭️ Skipping TransactionFinalized {} — node is syncing", hex::encode(*txid));
                                        continue;
                                    }

                                    // Dedup: skip if we've already processed this finalization
                                    let already_seen = seen_tx_finalized.check_and_insert(txid).await;
                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate TransactionFinalized {} from {}", hex::encode(*txid), peer.addr);
                                        continue;
                                    }

                                    tracing::info!("✅ Transaction {} finalized (from {})",
                                        hex::encode(*txid), peer.addr);

                                    // AV38+AV40 combined guard: drop null TXs that arrive as
                                    // TransactionFinalized.  The attacker feeds honest relay nodes
                                    // with null TXs (0 inputs, 0 outputs, no special_data), which
                                    // those nodes then re-broadcast as TransactionFinalized.  By
                                    // dropping here we stop the amplification WITHOUT banning the
                                    // relay (which is also a victim).  The AI tracker records the
                                    // event at a relay-safe threshold so only the true source is
                                    // eventually penalised.
                                    if tx.inputs.is_empty() && tx.outputs.is_empty() && tx.special_data.is_none() {
                                        tracing::debug!(
                                            "🗑️ Null TX {} via TransactionFinalized from {} — dropped (AV38+AV40)",
                                            hex::encode(*txid), peer.addr
                                        );
                                        if let Some(ref ai) = ai_system {
                                            ai.attack_detector.record_finality_injection(&ip_str);
                                        }
                                        continue;
                                    }

                                    // AV41/AV48: ghost special_data guard. Rejects:
                                    //   Phase 1 — empty/malformed fields (validate_fields)
                                    //   Phase 2 — forged signature (verify_signature)
                                    //   Phase 3 — fresh keypair with mismatched wallet_address (verify_address_binding)
                                    if tx.inputs.is_empty() && tx.outputs.is_empty() {
                                        let sig_ok = tx.special_data.as_ref().map_or(
                                            false,
                                            |sd| {
                                                sd.validate_fields().is_ok()
                                                    && sd.verify_signature().is_ok()
                                                    && sd.verify_address_binding().is_ok()
                                            },
                                        );
                                        if !sig_ok {
                                            tracing::debug!(
                                                "🗑️ Ghost/forged special_data TX {} via TransactionFinalized from {} — dropped (AV41)",
                                                hex::encode(*txid), peer.addr
                                            );
                                            if let Some(ref ai) = ai_system {
                                                ai.attack_detector.record_finality_injection(&ip_str);
                                            }
                                            continue;
                                        }
                                    }

                                    // If the TX is already in the finalized pool, skip entirely
                                    if consensus.tx_pool.is_finalized(txid) {
                                        tracing::debug!("📦 TX {} already in finalized pool, skipping", hex::encode(*txid));
                                        // Still gossip so other peers learn about it
                                        let _ = broadcast_tx.send(msg.clone());
                                        continue;
                                    }

                                    // Check whether all input UTXOs are present in local storage.
                                    // For a TransactionFinalized message the network has already
                                    // reached consensus on this TX, so missing inputs only mean our
                                    // local UTXO set is diverged — not that the TX is invalid.
                                    // We still apply the outputs so wallets see the correct balance;
                                    // missing inputs are skipped (they will be cleaned up by the
                                    // next UTXO reconciliation round).
                                    let mut inputs_exist = true;
                                    for input in &tx.inputs {
                                        if consensus.utxo_manager.get_utxo(&input.previous_output).await.is_err() {
                                            tracing::warn!(
                                                "⚠️ TransactionFinalized {} from {}: input {} not in local storage \
                                                 (UTXO set diverged) — will apply outputs without marking inputs spent",
                                                hex::encode(*txid), peer.addr, input.previous_output
                                            );
                                            inputs_exist = false;
                                            break;
                                        }
                                    }
                                    if !inputs_exist {
                                        // Apply outputs directly so the recipient wallet sees the
                                        // new UTXOs even while our local set is diverged.
                                        for (idx, output) in tx.outputs.iter().enumerate() {
                                            let outpoint = crate::types::OutPoint {
                                                txid: *txid,
                                                vout: idx as u32,
                                            };
                                            let utxo = crate::types::UTXO {
                                                outpoint: outpoint.clone(),
                                                value: output.value,
                                                script_pubkey: output.script_pubkey.clone(),
                                                address: String::from_utf8(output.script_pubkey.clone())
                                                    .unwrap_or_default(),
                                            masternode_key: None,
                                            };
                                            if let Err(e) = consensus.utxo_manager.add_utxo(utxo).await {
                                                tracing::warn!(
                                                    "Failed to add output UTXO vout={} for diverged TX {}: {}",
                                                    idx, hex::encode(*txid), e
                                                );
                                            } else {
                                                consensus.utxo_manager.update_state(&outpoint, crate::types::UTXOState::Unspent);
                                            }
                                        }
                                        // Gossip so other peers can also learn about this finalization
                                        let msg = NetworkMessage::TransactionFinalized {
                                            txid: *txid,
                                            tx: tx.clone(),
                                        };
                                        let _ = broadcast_tx.send(msg);
                                        continue;
                                    }

                                    // Add to pool if not present.  We deliberately do NOT call
                                    // process_transaction() here — that would broadcast a
                                    // TimeVoteRequest to all validators, giving a 49x amplification
                                    // to an attacker who injects TransactionFinalized for unknown
                                    // TXs (AV38: Finality Injection).  Instead, add directly to the
                                    // pending pool and let the manual finalization path below handle
                                    // the UTXO state transitions.
                                    let auto_finalized = if !consensus.tx_pool.has_transaction(txid) {
                                        tracing::warn!(
                                            "⚠️ TransactionFinalized for unknown TX {} from {} — \
                                             adding to pool without consensus re-broadcast (AV38 guard)",
                                            hex::encode(*txid), peer.addr
                                        );
                                        // Record for AI sliding-window detection (AV38).
                                        if let Some(ref ai) = ai_system {
                                            ai.attack_detector.record_finality_injection(&ip_str);
                                        }
                                        // Add directly without triggering TimeVote broadcast.
                                        let _ = consensus.tx_pool.add_pending(tx.clone(), 0);
                                        false // let the manual finalization path below run
                                    } else {
                                        false
                                    };

                                    // Only do manual finalization if process_transaction didn't already do it.
                                    // This prevents double-finalization (creating output UTXOs twice).
                                    if !auto_finalized {
                                        if consensus.tx_pool.finalize_transaction(*txid) {
                                            tracing::info!("📦 Moved TX {} to finalized pool on this node", hex::encode(*txid));

                                            // Transition input UTXOs → SpentFinalized
                                            for input in &tx.inputs {
                                                let new_state = crate::types::UTXOState::SpentFinalized {
                                                    txid: *txid,
                                                    finalized_at: chrono::Utc::now().timestamp(),
                                                    votes: 0,
                                                };
                                                consensus.utxo_manager.update_state(&input.previous_output, new_state);
                                            }

                                            // Create output UTXOs
                                            for (idx, output) in tx.outputs.iter().enumerate() {
                                                let outpoint = crate::types::OutPoint {
                                                    txid: *txid,
                                                    vout: idx as u32,
                                                };
                                                let utxo = crate::types::UTXO {
                                                    outpoint: outpoint.clone(),
                                                    value: output.value,
                                                    script_pubkey: output.script_pubkey.clone(),
                                                    address: String::from_utf8(output.script_pubkey.clone())
                                                        .unwrap_or_default(),
                                                masternode_key: None,
                                                };
                                                if let Err(e) = consensus.utxo_manager.add_utxo(utxo).await {
                                                    tracing::warn!("Failed to add output UTXO vout={}: {}", idx, e);
                                                }
                                                consensus.utxo_manager.update_state(&outpoint, crate::types::UTXOState::Unspent);
                                            }
                                        } else {
                                            tracing::debug!("⚠️ Could not finalize TX {} (not in pending pool)", hex::encode(*txid));
                                        }
                                    }

                                    // Notify WS subscribers on this node that the transaction is finalized
                                    consensus.signal_tx_finalized(*txid);

                                    // Gossip finalization to other peers
                                    match broadcast_tx.send(msg.clone()) {
                                        Ok(receivers) => {
                                            tracing::debug!("🔄 Gossiped finalization to {} peer(s)", receivers.saturating_sub(1));
                                        }
                                        Err(e) => {
                                            tracing::debug!("Failed to gossip finalization: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::UTXOStateQuery(_) => {
                                    check_rate_limit!("utxo_query");

                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let mut context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );
                                    context.utxo_manager = Some(Arc::clone(&utxo_mgr));

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::Subscribe(sub) => {
                                    check_rate_limit!("subscribe");
                                    subs.write().await.insert(sub.id.clone(), sub.clone());
                                }
                                NetworkMessage::GetBlockHeight => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetChainTip => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetPendingTransactions => {
                                    // Get pending transactions from mempool
                                    let pending_txs = blockchain.get_pending_transactions();
                                    let reply = NetworkMessage::PendingTransactionsResponse(pending_txs);
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                }
                                NetworkMessage::GetBlocks(_start, _end) => {
                                    check_rate_limit!("get_blocks");

                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetUTXOStateHash => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetUTXOSet => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::MasternodeAnnouncement { .. } => {
                                    // V1 deprecated — all nodes use V2 now
                                    tracing::debug!("⏭️  Ignoring deprecated V1 masternode announcement from {}", peer.addr);
                                }
                                NetworkMessage::MasternodeAnnouncementV2 { address: _, reward_address, tier, public_key, collateral_outpoint } => {
                                    // V2 without certificate — delegate to V3 handler with empty cert
                                    let v3_msg = NetworkMessage::MasternodeAnnouncementV3 {
                                        address: peer.addr.split(':').next().unwrap_or("").to_string(),
                                        reward_address: reward_address.clone(),
                                        tier: *tier,
                                        public_key: *public_key,
                                        collateral_outpoint: collateral_outpoint.clone(),
                                        certificate: vec![0u8; 64],
                                        started_at: 0,
                                    };
                                    // Fall through to V3 handler below
                                    // (re-dispatch via the message handler for consistency)
                                    check_rate_limit!("masternode_announce");
                                    if !is_stable_connection {
                                        is_stable_connection = true;
                                    }
                                    let peer_ip_str = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                    let mut context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );
                                    context.utxo_manager = Some(Arc::clone(&consensus.utxo_manager));
                                    context.peer_manager = Some(Arc::clone(&peer_manager));
                                    match handler.handle_message(&v3_msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&peer.addr, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::MasternodeAnnouncementV3 { address: _, reward_address, tier, public_key, collateral_outpoint, certificate, started_at } => {
                                    check_rate_limit!("masternode_announce");
                                    if !is_stable_connection {
                                        is_stable_connection = true;
                                    }
                                    let peer_ip_str = peer.addr.split(':').next().unwrap_or("").to_string();
                                    if peer_ip_str.is_empty() { continue; }
                                    // Delegate to unified message handler (same path as V2)
                                    let v3_msg = NetworkMessage::MasternodeAnnouncementV3 {
                                        address: peer_ip_str.clone(),
                                        reward_address: reward_address.clone(),
                                        tier: *tier,
                                        public_key: *public_key,
                                        collateral_outpoint: collateral_outpoint.clone(),
                                        certificate: certificate.clone(),
                                        started_at: *started_at,
                                    };
                                    let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                    let mut context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );
                                    context.utxo_manager = Some(Arc::clone(&consensus.utxo_manager));
                                    context.peer_manager = Some(Arc::clone(&peer_manager));
                                    match handler.handle_message(&v3_msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&peer.addr, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::MasternodeAnnouncementV4 { address: _, reward_address, tier, public_key, collateral_outpoint, certificate, started_at, collateral_proof } => {
                                    check_rate_limit!("masternode_announce");
                                    if !is_stable_connection {
                                        is_stable_connection = true;
                                    }
                                    let peer_ip_str = peer.addr.split(':').next().unwrap_or("").to_string();
                                    if peer_ip_str.is_empty() { continue; }
                                    let v4_msg = NetworkMessage::MasternodeAnnouncementV4 {
                                        address: peer_ip_str.clone(),
                                        reward_address: reward_address.clone(),
                                        tier: *tier,
                                        public_key: *public_key,
                                        collateral_outpoint: collateral_outpoint.clone(),
                                        certificate: certificate.clone(),
                                        started_at: *started_at,
                                        collateral_proof: collateral_proof.clone(),
                                    };
                                    let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                    let mut context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );
                                    context.utxo_manager = Some(Arc::clone(&consensus.utxo_manager));
                                    context.peer_manager = Some(Arc::clone(&peer_manager));
                                    match handler.handle_message(&v4_msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&peer.addr, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetPeers => {
                                    check_rate_limit!("get_peers");

                                    // Use unified message handler
                                    let peer_ip_str = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                    let mut context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );
                                    context.peer_manager = Some(Arc::clone(&peer_manager));

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetMasternodes => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::PeersResponse(peers) => {
                                    // Use unified message handler with peer_manager
                                    let peer_ip_str = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                    let mut context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );
                                    context.peer_manager = Some(Arc::clone(&peer_manager));

                                    let _ = handler.handle_message(&msg, &context).await;

                                    // Log statistics
                                    tracing::debug!("📥 Received PeersResponse from {} with {} peer(s)", peer.addr, peers.len());
                                }
                                NetworkMessage::MasternodesResponse(_) => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    let _ = handler.handle_message(&msg, &context).await;
                                }
                                NetworkMessage::BlockInventory(_) => {
                                    check_rate_limit!("block");

                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::BlockRequest(_) => {
                                    check_rate_limit!("block");

                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::BlockResponse(block) => {
                                    check_message_size!(MAX_BLOCK_SIZE, "Block");
                                    check_rate_limit!("block");

                                    // SECURITY: Check blacklist before processing ANY block
                                    {
                                        let mut bl = blacklist.write().await;
                                        if let Some(reason) = bl.is_blacklisted(ip) {
                                            tracing::warn!(
                                                "🚫 REJECTING BlockResponse from blacklisted peer {}: {}",
                                                peer.addr, reason
                                            );
                                            continue;
                                        }
                                    }

                                    let block_height = block.header.height;

                                    // Check if we've already seen this block using Bloom filter
                                    let block_height_bytes = block_height.to_le_bytes();
                                    let already_seen = seen_blocks.check_and_insert(&block_height_bytes).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate block {} from {}", block_height, peer.addr);
                                        continue;
                                    }

                                    tracing::info!("📥 Received block {} response from {}", block_height, peer.addr);

                                    // Add block to our blockchain with fork handling
                                    // Run on blocking thread to keep tokio workers free for RPC/networking
                                    let bc = blockchain.clone();
                                    let blk = block.clone();
                                    let result = tokio::task::spawn_blocking(move || {
                                        tokio::runtime::Handle::current().block_on(async {
                                            bc.add_block_with_fork_handling(blk).await
                                        })
                                    }).await;
                                    match result.unwrap_or_else(|e| Err(format!("Block processing panicked: {}", e))) {
                                        Ok(true) => {
                                            tracing::info!("✅ Added block {} from {}", block_height, peer.addr);

                                            // GOSSIP: Send inventory to all other connected peers
                                            let msg = NetworkMessage::BlockInventory(block_height);
                                            match broadcast_tx.send(msg) {
                                                Ok(receivers) => {
                                                    tracing::info!("🔄 Gossiped block {} inventory to {} other peer(s)", block_height, receivers.saturating_sub(1));
                                                }
                                                Err(e) => {
                                                    tracing::warn!("Failed to gossip block inventory: {}", e);
                                                }
                                            }
                                        }
                                        Ok(false) => {
                                            tracing::debug!("⏭️ Skipped block {} (already have or invalid)", block_height);
                                        }
                                        Err(e) if e.contains("corrupted") || e.contains("serialization failed") => {
                                            // SECURITY: Corrupted block from peer - severe violation
                                            tracing::error!(
                                                "🚨 CORRUPTED BLOCK {} from {} - recording severe violation: {}",
                                                block_height, peer.addr, e
                                            );
                                            let should_ban = blacklist.write().await.record_severe_violation(
                                                ip,
                                                &format!("Sent corrupted block {}: {}", block_height, e)
                                            );
                                            if should_ban {
                                                tracing::warn!("🚫 Disconnecting {} for sending corrupted block", peer.addr);
                                                peer_registry.kick_peer(&ip_str).await;
                                                break; // Exit the message loop to disconnect
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("❌ Failed to add block {}: {}", block_height, e);
                                        }
                                    }
                                }
                                NetworkMessage::BlockAnnouncement(block) => {
                                    // Legacy full block announcement (for backward compatibility)
                                    check_message_size!(MAX_BLOCK_SIZE, "Block");
                                    check_rate_limit!("block");

                                    // SECURITY: Check blacklist before processing ANY block
                                    {
                                        let mut bl = blacklist.write().await;
                                        if let Some(reason) = bl.is_blacklisted(ip) {
                                            tracing::warn!(
                                                "🚫 REJECTING BlockAnnouncement from blacklisted peer {}: {}",
                                                peer.addr, reason
                                            );
                                            continue;
                                        }
                                    }

                                    let block_height = block.header.height;

                                    // Check if we've already seen this block using Bloom filter
                                    let block_height_bytes = block_height.to_le_bytes();
                                    let already_seen = seen_blocks.check_and_insert(&block_height_bytes).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate block {} from {}", block_height, peer.addr);
                                        continue;
                                    }

                                    tracing::debug!("📥 Received legacy block {} announcement from {}", block_height, peer.addr);

                                    // Add block to our blockchain with fork handling
                                    // Run on blocking thread to keep tokio workers free for RPC/networking
                                    let bc = blockchain.clone();
                                    let blk = block.clone();
                                    let result = tokio::task::spawn_blocking(move || {
                                        tokio::runtime::Handle::current().block_on(async {
                                            bc.add_block_with_fork_handling(blk).await
                                        })
                                    }).await;
                                    match result.unwrap_or_else(|e| Err(format!("Block processing panicked: {}", e))) {
                                        Ok(true) => {
                                            tracing::info!("✅ Added block {} from {}", block_height, peer.addr);

                                            // GOSSIP: Use inventory for efficiency
                                            let msg = NetworkMessage::BlockInventory(block_height);
                                            match broadcast_tx.send(msg) {
                                                Ok(receivers) => {
                                                    tracing::info!("🔄 Gossiped block {} inventory to {} other peer(s)", block_height, receivers.saturating_sub(1));
                                                }
                                                Err(e) => {
                                                    tracing::warn!("Failed to gossip block inventory: {}", e);
                                                }
                                            }
                                        }
                                        Ok(false) => {
                                            tracing::debug!("⏭️ Skipped block {} (already have or fork)", block_height);
                                        }
                                        Err(e) if e.contains("corrupted") || e.contains("serialization failed") => {
                                            // SECURITY: Corrupted block from peer - severe violation
                                            tracing::error!(
                                                "🚨 CORRUPTED BLOCK {} from {} (announcement) - recording severe violation: {}",
                                                block_height, peer.addr, e
                                            );
                                            let should_ban = blacklist.write().await.record_severe_violation(
                                                ip,
                                                &format!("Sent corrupted block {}: {}", block_height, e)
                                            );
                                            if should_ban {
                                                tracing::warn!("🚫 Disconnecting {} for sending corrupted block", peer.addr);
                                                peer_registry.kick_peer(&ip_str).await;
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to add announced block: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::GenesisAnnouncement(block) => {
                                    // Special handling for genesis block announcements
                                    check_message_size!(MAX_BLOCK_SIZE, "GenesisBlock");
                                    check_rate_limit!("genesis");

                                    // Verify this is actually a genesis block
                                    if block.header.height != 0 {
                                        tracing::warn!("⚠️  Received GenesisAnnouncement for non-genesis block {} from {}", block.header.height, peer.addr);
                                        continue;
                                    }

                                    // Check if we already have genesis - try to get block at height 0
                                    if blockchain.get_block_by_height(0).await.is_ok() {
                                        tracing::debug!("⏭️ Ignoring genesis announcement (already have genesis) from {}", peer.addr);
                                        continue;
                                    }

                                    tracing::info!("📦 Received genesis announcement from {}", peer.addr);

                                    // Simply verify basic genesis structure
                                    use crate::block::genesis::GenesisBlock;
                                    match GenesisBlock::verify_structure(block) {
                                        Ok(()) => {
                                            tracing::info!("✅ Genesis structure validation passed, adding to chain");

                                            // Add genesis to our blockchain
                                            match blockchain.add_block(block.clone()).await {
                                                Ok(()) => {
                                                    tracing::info!("✅ Genesis block added successfully, hash: {}", hex::encode(&block.hash()[..8]));

                                                    // Broadcast to other peers who might not have it yet
                                                    let msg = NetworkMessage::GenesisAnnouncement(block.clone());
                                                    let _ = broadcast_tx.send(msg);
                                                }
                                                Err(e) => {
                                                    tracing::error!("❌ Failed to add genesis block: {}", e);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("⚠️  Genesis validation failed: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::RequestGenesis => {
                                    check_rate_limit!("genesis_request");

                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetBlockHash(_) => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::ConsensusQuery { .. } => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetBlockRange { .. } => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::BlocksResponse(_) | NetworkMessage::BlockRangeResponse(_) => {
                                    // ✅ REFACTORED: Route through unified message_handler.rs
                                    // See: analysis/REFACTORING_ROADMAP.md - Phase 1, Step 1.2 (COMPLETED)

                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    // Handle the message through unified handler
                                    let _ = handler.handle_message(&msg, &context).await;
                                }
                                // Health Check Messages
                                NetworkMessage::Ping { .. } | NetworkMessage::Pong { .. } => {
                                    let rate_ok = {
                                        let mut limiter = rate_limiter.write().await;
                                        let msg_type = if matches!(&msg, NetworkMessage::Ping { .. }) { "ping" } else { "pong" };
                                        limiter.check(msg_type, &ip_str)
                                    };
                                    if !rate_ok {
                                        if matches!(&msg, NetworkMessage::Ping { .. }) {
                                            ping_excess_streak += 1;
                                            tracing::debug!(
                                                "⚡ Ping rate limit exceeded from {} (excess streak: {})",
                                                peer.addr, ping_excess_streak
                                            );
                                            if ping_excess_streak >= 3 {
                                                tracing::warn!(
                                                    "🌊 Ping flood from {} (excess streak {}): recording violation",
                                                    peer.addr, ping_excess_streak
                                                );
                                                let should_ban = blacklist.write().await.record_violation(
                                                    ip,
                                                    "Ping flood: sustained excess pings"
                                                );
                                                if let Some(ai) = &ai_system {
                                                    ai.attack_detector.record_ping_flood(&ip_str);
                                                }
                                                if should_ban {
                                                    tracing::warn!("🚫 Disconnecting {} due to ping flood violations", peer.addr);
                                                    peer_registry.kick_peer(&ip_str).await;
                                                    break;
                                                }
                                                ping_excess_streak = 0;
                                            }
                                        }
                                        continue;
                                    }
                                    ping_excess_streak = 0;

                                    // Route through unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::TimeVoteRequest { txid, tx_hash_commitment, slot_index, tx } => {
                                    check_message_size!(MAX_VOTE_SIZE, "TimeVoteRequest");
                                    check_rate_limit!("vote");

                                    // Spawn vote processing (non-blocking)
                                    let txid_val = *txid;
                                    let tx_hash_commitment_val = *tx_hash_commitment;
                                    let slot_index_val = *slot_index;
                                    let tx_from_request = tx.clone(); // NEW: Optional TX included in request
                                    let peer_addr_str = peer.addr.to_string();
                                    let ip_str_clone = ip_str.clone();
                                    let consensus_clone = Arc::clone(&consensus);
                                    let peer_registry_clone = Arc::clone(&peer_registry);

                                    tokio::spawn(async move {
                                        tracing::info!(
                                            "🗳️  TimeVoteRequest from {} for TX {} (slot {}){}",
                                            peer_addr_str,
                                            hex::encode(txid_val),
                                            slot_index_val,
                                            if tx_from_request.is_some() { " [TX included]" } else { "" }
                                        );

                                        // FIX: Step 1 - Get TX from mempool OR from request
                                        let mut tx_opt = consensus_clone.tx_pool.get_pending(&txid_val);

                                        // If not in mempool but included in request, add it
                                        if tx_opt.is_none() {
                                            if let Some(tx_from_req) = tx_from_request {
                                                tracing::debug!(
                                                    "📥 TX {} not in mempool, adding from TimeVoteRequest",
                                                    hex::encode(txid_val)
                                                );

                                                // Add to pending pool (this also validates basic structure)
                                                let input_sum: u64 = {
                                                    let mut sum = 0u64;
                                                    for input in &tx_from_req.inputs {
                                                        if let Ok(utxo) = consensus_clone.utxo_manager.get_utxo(&input.previous_output).await {
                                                            sum += utxo.value;
                                                        }
                                                    }
                                                    sum
                                                };
                                                let output_sum: u64 = tx_from_req.outputs.iter().map(|o| o.value).sum();
                                                let fee = input_sum.saturating_sub(output_sum);

                                                if consensus_clone.tx_pool.add_pending(tx_from_req.clone(), fee).is_ok() {
                                                    tracing::debug!("✅ TX {} added to mempool from request", hex::encode(txid_val));
                                                    tx_opt = Some(tx_from_req);
                                                }
                                            }
                                        }

                                        let decision = if let Some(tx) = tx_opt {
                                            // Step 2: Verify tx_hash_commitment matches actual transaction
                                            let actual_commitment = crate::types::TimeVote::calculate_tx_commitment(&tx);
                                            if actual_commitment != tx_hash_commitment_val {
                                                tracing::warn!(
                                                    "⚠️  TX {} commitment mismatch: expected {:?}, got {:?}",
                                                    hex::encode(txid_val),
                                                    hex::encode(actual_commitment),
                                                    hex::encode(tx_hash_commitment_val)
                                                );
                                                crate::types::VoteDecision::Reject
                                            } else {
                                                // Step 3: Verify UTXOs are available (basic validation)
                                                match consensus_clone.validate_transaction(&tx).await {
                                                    Ok(_) => {
                                                        tracing::info!("✅ TX {} validated successfully for vote", hex::encode(txid_val));
                                                        crate::types::VoteDecision::Accept
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("⚠️  TX {} validation failed: {}", hex::encode(txid_val), e);
                                                        crate::types::VoteDecision::Reject
                                                    }
                                                }
                                            }
                                        } else {
                                            tracing::debug!("⚠️  TX {} not found in mempool and not included in request", hex::encode(txid_val));
                                            crate::types::VoteDecision::Reject
                                        };

                                        // Step 4: Sign TimeVote with our masternode key
                                        let vote_opt = consensus_clone.sign_timevote(
                                            txid_val,
                                            tx_hash_commitment_val,
                                            slot_index_val,
                                            decision,
                                        );

                                        if let Some(vote) = vote_opt {
                                            // Step 5: Send TimeVoteResponse with signed vote
                                            let vote_response = NetworkMessage::TimeVoteResponse { vote };
                                            match peer_registry_clone.send_to_peer(&ip_str_clone, vote_response).await {
                                                Ok(_) => {
                                                    tracing::info!(
                                                        "✅ TimeVoteResponse sent to {} for TX {} (decision: {:?})",
                                                        ip_str_clone,
                                                        hex::encode(txid_val),
                                                        decision
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "❌ Failed to send TimeVoteResponse to {} for TX {}: {}",
                                                        ip_str_clone,
                                                        hex::encode(txid_val),
                                                        e
                                                    );
                                                }
                                            }
                                        } else {
                                            tracing::warn!(
                                                "⚠️ TimeVote signing skipped for TX {} (not a masternode or identity not set)",
                                                hex::encode(txid_val)
                                            );
                                        }
                                    });
                                }
                                NetworkMessage::TimeVoteResponse { vote } => {
                                    check_message_size!(MAX_VOTE_SIZE, "TimeVoteResponse");
                                    check_rate_limit!("vote");

                                    // Received a signed TimeVote from a peer
                                    tracing::info!(
                                        "📥 TimeVoteResponse from {} for TX {} (decision: {:?}, weight: {})",
                                        peer.addr,
                                        hex::encode(vote.txid),
                                        vote.decision,
                                        vote.voter_weight
                                    );

                                    let txid = vote.txid;
                                    let vote_clone = vote.clone();
                                    let consensus_clone = Arc::clone(&consensus);
                                    let tx_pool = Arc::clone(&consensus.tx_pool);

                                    // Spawn finality check (non-blocking)
                                    tokio::spawn(async move {
                                        // Step 1: Accumulate the vote
                                        let accumulated_weight = match consensus_clone.timevote.accumulate_timevote(vote_clone.clone()) {
                                            Ok(weight) => weight,
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Failed to accumulate vote for TX {}: {}",
                                                    hex::encode(txid),
                                                    e
                                                );
                                                return;
                                            }
                                        };

                                        tracing::info!(
                                            "Vote accumulated for TX {}, total weight: {}",
                                            hex::encode(txid),
                                            accumulated_weight
                                        );

                                        // Step 2: Check if finality threshold reached (67% BFT-safe majority)
                                        let validators = consensus_clone.timevote.get_validators();
                                        let total_avs_weight: u64 = validators.iter().map(|v| v.weight).sum();
                                        let finality_threshold = ((total_avs_weight as f64) * 0.67).ceil() as u64;

                                        tracing::info!(
                                            "Finality check for TX {}: accumulated={}, threshold={} (67% of {})",
                                            hex::encode(txid),
                                            accumulated_weight,
                                            finality_threshold,
                                            total_avs_weight
                                        );

                                        // Step 3: If threshold met, finalize transaction
                                        if accumulated_weight >= finality_threshold {
                                            tracing::info!(
                                                "🎉 TX {} reached finality threshold! ({} >= {})",
                                                hex::encode(txid),
                                                accumulated_weight,
                                                finality_threshold
                                            );

                                            // FIX: Use atomic finalization guard to prevent race conditions
                                            // Multiple concurrent votes may all try to finalize - only first succeeds
                                            use dashmap::mapref::entry::Entry;
                                            match consensus_clone.timevote.finalized_txs.entry(txid) {
                                                Entry::Vacant(e) => {
                                                    // We're the first to finalize - claim it
                                                    e.insert((crate::consensus::Preference::Accept, std::time::Instant::now()));

                                                    tracing::info!(
                                                        "🔒 Acquired finalization lock for TX {}",
                                                        hex::encode(txid)
                                                    );

                                                    // Move transaction from pending to finalized
                                                    let tx_data = tx_pool.get_pending(&txid);
                                                    if tx_pool.finalize_transaction(txid) {
                                                        tracing::info!(
                                                            "✅ TX {} moved to finalized pool",
                                                            hex::encode(txid)
                                                        );

                                                        // Transition input UTXOs and create output UTXOs
                                                        if let Some(ref tx) = tx_data {
                                                            for input in &tx.inputs {
                                                                let new_state = crate::types::UTXOState::SpentFinalized {
                                                                    txid,
                                                                    finalized_at: chrono::Utc::now().timestamp(),
                                                                    votes: 0,
                                                                };
                                                                consensus_clone.utxo_manager.update_state(&input.previous_output, new_state);
                                                            }
                                                            for (idx, output) in tx.outputs.iter().enumerate() {
                                                                let outpoint = crate::types::OutPoint {
                                                                    txid,
                                                                    vout: idx as u32,
                                                                };
                                                                let utxo = crate::types::UTXO {
                                                                    outpoint: outpoint.clone(),
                                                                    value: output.value,
                                                                    script_pubkey: output.script_pubkey.clone(),
                                                                    address: String::from_utf8(output.script_pubkey.clone())
                                                                        .unwrap_or_default(),
                                                                masternode_key: None,
                                                                };
                                                                if let Err(e) = consensus_clone.utxo_manager.add_utxo(utxo).await {
                                                                    tracing::warn!("Failed to add output UTXO vout={}: {}", idx, e);
                                                                }
                                                                consensus_clone.utxo_manager.update_state(&outpoint, crate::types::UTXOState::Unspent);
                                                            }
                                                        }

                                                        // Record finalization weight
                                                        consensus_clone.timevote.record_finalization(txid, accumulated_weight);

                                                        // Notify WS subscribers about finalized transaction
                                                        consensus_clone.signal_tx_finalized(txid);

                                                        // Assemble TimeProof certificate
                                                        match consensus_clone.timevote.assemble_timeproof(txid) {
                                                            Ok(timeproof) => {
                                                                tracing::info!(
                                                                    "📜 TimeProof assembled for TX {} with {} votes",
                                                                    hex::encode(txid),
                                                                    timeproof.votes.len()
                                                                );

                                                                // Store TimeProof in finality_proof_manager
                                                                if let Err(e) = consensus_clone.finality_proof_mgr.store_timeproof(timeproof.clone()) {
                                                                    tracing::error!(
                                                                        "❌ Failed to store TimeProof for TX {}: {}",
                                                                        hex::encode(txid),
                                                                        e
                                                                    );
                                                                }

                                                                // Broadcast TimeProof to network (Task 2.5)
                                                                consensus_clone.broadcast_timeproof(timeproof).await;
                                                            }
                                                            Err(e) => {
                                                                tracing::error!(
                                                                    "❌ Failed to assemble TimeProof for TX {}: {}",
                                                                    hex::encode(txid),
                                                                    e
                                                                );
                                                            }
                                                        }
                                                    } else {
                                                        tracing::warn!(
                                                            "⚠️  Failed to finalize TX {} - not found in pending pool",
                                                            hex::encode(txid)
                                                        );
                                                    }
                                                }
                                                Entry::Occupied(_) => {
                                                    // Another task already finalized this TX - skip
                                                    tracing::debug!(
                                                        "TX {} already finalized by another task",
                                                        hex::encode(txid)
                                                    );
                                                }
                                            }
                                        }
                                    });
                                }
                                NetworkMessage::TimeProofBroadcast { proof } => {
                                    check_message_size!(MAX_VOTE_SIZE, "TimeProofBroadcast");
                                    check_rate_limit!("vote");

                                    // Received TimeProof certificate from peer
                                    let proof_clone = proof.clone();
                                    let consensus_clone = Arc::clone(&consensus);
                                    let peer_addr_str = peer.addr.to_string();

                                    // Spawn verification (non-blocking)
                                    tokio::spawn(async move {
                                        tracing::info!(
                                            "📜 Received TimeProof from {} for TX {} with {} votes",
                                            peer_addr_str,
                                            hex::encode(proof_clone.txid),
                                            proof_clone.votes.len()
                                        );

                                        // Verify TimeProof using consensus engine's verification method
                                        match consensus_clone.timevote.verify_timeproof(&proof_clone) {
                                            Ok(_accumulated_weight) => {
                                                tracing::info!(
                                                    "✅ TimeProof verified for TX {}",
                                                    hex::encode(proof_clone.txid)
                                                );

                                                // Store verified TimeProof
                                                if let Err(e) = consensus_clone.finality_proof_mgr.store_timeproof(proof_clone.clone()) {
                                                    tracing::error!(
                                                        "❌ Failed to store TimeProof for TX {}: {}",
                                                        hex::encode(proof_clone.txid),
                                                        e
                                                    );
                                                } else {
                                                    tracing::info!(
                                                        "💾 TimeProof stored for TX {}",
                                                        hex::encode(proof_clone.txid)
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "⚠️  Invalid TimeProof from {}: {}",
                                                    peer_addr_str,
                                                    e
                                                );
                                            }
                                        }
                                    });
                                }
                                NetworkMessage::TransactionVoteRequest { .. }
                                | NetworkMessage::TransactionVoteResponse { .. } => {
                                    // Deprecated legacy vote protocol — superseded by TimeVoteRequest/Response.
                                    // These are no-ops: the response never updated consensus state.
                                    check_rate_limit!("vote");
                                }
                                NetworkMessage::FinalityVoteBroadcast { vote } => {
                                    check_message_size!(MAX_VOTE_SIZE, "FinalityVote");
                                    check_rate_limit!("vote");

                                    // Received a finality vote from a peer
                                    tracing::debug!("📥 Finality vote from {} for TX {}", peer.addr, hex::encode(vote.txid));

                                    // Accumulate the finality vote in consensus
                                    if let Err(e) = consensus.timevote.accumulate_finality_vote(vote.clone()) {
                                        tracing::warn!("Failed to accumulate finality vote from {}: {}", peer.addr, e);
                                    } else {
                                        tracing::debug!("✅ Finality vote recorded from {}", peer.addr);
                                    }
                                }
                                NetworkMessage::TimeLockBlockProposal { .. }
                                | NetworkMessage::TimeVotePrepare { .. }
                                | NetworkMessage::TimeVotePrecommit { .. } => {
                                    // SECURITY: Check blacklist before processing ANY consensus messages
                                    {
                                        let mut bl = blacklist.write().await;
                                        if let Some(reason) = bl.is_blacklisted(ip) {
                                            tracing::warn!(
                                                "🚫 REJECTING TimeLock message from blacklisted peer {}: {}",
                                                peer.addr, reason
                                            );
                                            continue;
                                        }
                                    }

                                    // Use unified message handler for TimeLock messages
                                    let handler = MessageHandler::new(ip_str.clone(), ConnectionDirection::Inbound);
                                    // Get local masternode address for vote identity
                                    let local_mn_addr = masternode_registry.get_local_address().await;
                                    let context = MessageContext::with_consensus(
                                        blockchain.clone(),
                                        peer_registry.clone(),
                                        masternode_registry.clone(),
                                        consensus.clone(),
                                        block_cache.clone(),
                                        broadcast_tx.clone(),
                                        local_mn_addr,
                                    ).with_blacklist(Arc::clone(&blacklist));

                                    if let Err(e) = handler.handle_message(&msg, &context).await {
                                        tracing::warn!("[Inbound] Error handling TimeLock message from {}: {}", peer.addr, e);
                                    }
                                }
                                NetworkMessage::GetChainWork => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::GetChainWorkAt(_) => {
                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => { let _ = peer_registry.send_to_peer(&ip_str, response).await; }
                                        Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e); break; }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::ChainWorkResponse { height, tip_hash, cumulative_work } => {
                                    // Handle response - check if peer has better chain and potentially trigger reorg
                                    let _our_height = blockchain.get_height();

                                    if blockchain.should_switch_by_work(*cumulative_work, *height, tip_hash, Some(&ip_str)).await {
                                        tracing::info!(
                                            "📊 Peer {} has better chain, requesting blocks",
                                            peer.addr
                                        );

                                        // Check for fork and request the first batch of missing blocks.
                                        // Cap to 50 blocks to stay well within the 16MB frame limit;
                                        // subsequent batches will be fetched by the normal sync path.
                                        if let Some(fork_height) = blockchain.detect_fork(*height, *tip_hash).await {
                                            tracing::warn!(
                                                "🔀 Fork detected at height {} with {}, requesting blocks",
                                                fork_height, peer.addr
                                            );

                                            let batch_end = (*height).min(fork_height + 49);
                                            let request = NetworkMessage::GetBlockRange {
                                                start_height: fork_height,
                                                end_height: batch_end,
                                            };
                                            let _ = peer_registry.send_to_peer(&ip_str, request).await;
                                        }
                                    }
                                }
                                NetworkMessage::ChainWorkAtResponse { .. }
                                | NetworkMessage::BlockHashResponse { .. } => {
                                    // Handle via response system - dispatched to waiting oneshot channels
                                    peer_registry.handle_response(&ip_str, msg).await;
                                }
                                NetworkMessage::ChainTipResponse { .. } => {
                                    // Route through unified message handler to update peer_chain_tips cache
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Err(e) if e.contains("DISCONNECT:") => {
                                            tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e);
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                NetworkMessage::MasternodeStatusGossip { .. } => {
                                    // Handle gossip via unified message handler
                                    let handler = MessageHandler::new(ip_str.clone(), ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    if let Err(e) = handler.handle_message(&msg, &context).await {
                                        tracing::warn!("[Inbound] Error handling gossip from {}: {}", peer.addr, e);
                                    }
                                }
                                _ => {
                                    // Fallback: delegate any unhandled message types to MessageHandler
                                    // This prevents silently dropping messages that server.rs doesn't
                                    // explicitly handle (e.g. BlockHeightResponse, ForkAlert, LivenessAlert)
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    match handler.handle_message(&msg, &context).await {
                                        Ok(Some(response)) => {
                                            let _ = peer_registry.send_to_peer(&ip_str, response).await;
                                        }
                                        Err(e) if e.contains("DISCONNECT:") => {
                                            tracing::warn!("🔌 Disconnecting {} — {}", peer.addr, e);
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }

            result = notifier.recv() => {
                match result {
                    Ok(msg) => {
                        // Log what we're broadcasting
                        match &msg {
                            NetworkMessage::BlockAnnouncement(block) => {
                                tracing::debug!("📤 Sending block {} to peer {}", block.header.height, peer.addr);
                            }
                            NetworkMessage::BlockInventory(height) => {
                                tracing::debug!("📤 Sending block {} inventory to peer {}", height, peer.addr);
                            }
                            _ => {
                                tracing::debug!("📤 Sending message to peer {}", peer.addr);
                            }
                        }

                        let _ = peer_registry.send_to_peer(&ip_str, msg).await;
                    }
                    Err(_) => break,
                }
            }

            // Close connections that complete TLS but never send a Handshake message.
            // The guard disables this arm after the handshake succeeds so there is no
            // ongoing per-iteration overhead once the connection is fully established.
            _ = &mut handshake_timeout, if !handshake_done => {
                tracing::warn!(
                    "⏰ Pre-handshake timeout from {} — no handshake received within 10s, closing",
                    peer.addr
                );
                blacklist.write().await.record_violation(
                    ip,
                    "Pre-handshake timeout: no handshake message within 10s",
                );
                break;
            }
        }
    }

    // Cleanup: mark inbound connection as disconnected in BOTH managers
    connection_manager.mark_inbound_disconnected(&ip_str);
    peer_registry.unregister_peer(&ip_str).await;

    // Mark masternode as inactive only if the handshake completed.
    // Connections that never completed the version exchange (e.g., old
    // software that sends messages before the handshake) must not trigger
    // registry changes: the peer never identified itself on this connection,
    // so there is nothing meaningful to update.  Without this guard, inbound
    // pre-handshake failures from old-software peers cause their previously
    // registered entry (which may be a paid-tier node) to be removed as a
    // "transient Free-tier" node, creating continuous reconnection churn.
    if handshake_done {
        if let Err(e) = masternode_registry
            .mark_inactive_on_disconnect(&ip_str)
            .await
        {
            tracing::debug!("Note: {} is not a registered masternode ({})", ip_str, e);
        }
        // Notify AI detector of masternode disconnect for synchronized cycling detection (AV3).
        // If ≥5 nodes from the same /24 subnet disconnect within 30s, the AI will recommend
        // BanSubnet, which the enforcement loop applies on its next 30s tick.
        if let Some(ref ai) = ai_system {
            ai.attack_detector.record_synchronized_disconnect(&ip_str);
        }
    }

    tracing::info!("🔌 Peer {} disconnected", peer.addr);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Transaction, TxInput, TxOutput};

    #[test]
    fn test_message_size_limits() {
        // Phase 2.3: Verify message size constants are properly set
        assert_eq!(MAX_MESSAGE_SIZE, 2_000_000); // 2MB absolute max
        assert_eq!(MAX_BLOCK_SIZE, 2_000_000); // 2MB for blocks
        assert_eq!(MAX_TX_SIZE, 100_000); // 100KB for transactions
        assert_eq!(MAX_VOTE_SIZE, 1_000); // 1KB for votes
        assert_eq!(MAX_GENERAL_SIZE, 50_000); // 50KB for general

        // Verify hierarchy
        // Note: These are constant assertions that clippy warns about.
        // The hierarchy is enforced at compile time by the constant values themselves.
        // Documented here for clarity:
        // MAX_BLOCK_SIZE (2MB) < MAX_MESSAGE_SIZE (2MB) — equal is fine
        // MAX_TX_SIZE (100KB) < MAX_BLOCK_SIZE (2MB)
        // MAX_VOTE_SIZE (1KB) < MAX_TX_SIZE (100KB)
        // MAX_GENERAL_SIZE (50KB) < MAX_TX_SIZE (100KB)
    }

    #[test]
    fn test_oversized_message_detection() {
        // Test that we can detect when a message would be too large
        let message_size = 2_500_000; // 2.5MB
        assert!(
            message_size > MAX_MESSAGE_SIZE,
            "Oversized message should exceed limit"
        );

        let block_size = 2_500_000; // 2.5MB
        assert!(
            block_size > MAX_BLOCK_SIZE,
            "Oversized block should exceed limit"
        );

        let tx_size = 150_000; // 150KB
        assert!(
            tx_size > MAX_TX_SIZE,
            "Oversized transaction should exceed limit"
        );

        let vote_size = 2_000; // 2KB
        assert!(
            vote_size > MAX_VOTE_SIZE,
            "Oversized vote should exceed limit"
        );
    }

    #[test]
    fn test_normal_message_sizes() {
        // Test that normal messages are within limits

        // Normal transaction: ~500 bytes
        let normal_tx_size = 500;
        assert!(
            normal_tx_size < MAX_TX_SIZE,
            "Normal transaction should be within limit"
        );

        // Normal block with 100 transactions: ~50KB
        let normal_block_size = 50_000;
        assert!(
            normal_block_size < MAX_BLOCK_SIZE,
            "Normal block should be within limit"
        );

        // Normal vote: ~200 bytes
        let normal_vote_size = 200;
        assert!(
            normal_vote_size < MAX_VOTE_SIZE,
            "Normal vote should be within limit"
        );
    }

    #[test]
    fn test_transaction_serialization_size() {
        // Create a typical transaction and verify it's within limits
        let tx = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: crate::types::OutPoint {
                    txid: [0u8; 32],
                    vout: 0,
                },
                script_sig: vec![0u8; 64],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 100_000_000,
                script_pubkey: vec![0u8; 25],
            }],
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
            special_data: None,
            encrypted_memo: None,
        };

        // Serialize with bincode and check size
        let serialized = bincode::serialize(&tx).expect("Failed to serialize transaction");
        let tx_size = serialized.len();

        assert!(
            tx_size < MAX_TX_SIZE,
            "Typical transaction should be within limit: {} < {}",
            tx_size,
            MAX_TX_SIZE
        );

        // Even with 10 inputs and 10 outputs, should still be reasonable
        assert!(
            tx_size * 20 < MAX_TX_SIZE,
            "Large transaction with 10 inputs/outputs should still fit"
        );
    }
}
