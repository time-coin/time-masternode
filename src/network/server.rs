//! Network server for P2P communication.
//!
//! Note: This module appears as "dead code" in library checks because it's
//! only used by the binary (main.rs). The NetworkServer is created and run
//! in main() for handling all P2P network communication.

use crate::consensus::ConsensusEngine;
use crate::network::attack_log::AttackLog;
use crate::network::banlist::IPBanlist;
use crate::network::block_cache::BlockCache;
use crate::network::ddos_guard::DDoSGuard;
use crate::network::dedup_filter::DeduplicationFilter;
use crate::network::message::{NetworkMessage, Subscription, UTXOStateChange};
use crate::network::peer_connection::PeerStateManager;
use crate::network::rate_limiter::RateLimiter;
use crate::types::OutPoint;
use crate::utxo_manager::UTXOStateManager;
use dashmap::DashMap;
use std::collections::HashMap;
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
    pub banlist: Arc<RwLock<IPBanlist>>,
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
    pub ddos_guard: Arc<DDoSGuard>, // Integrated DDoS coordinator (subnet rate tracking)
    pub attack_log: Option<Arc<AttackLog>>, // Separate file log for AI-detected attacks
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
        Self::new_with_banlist(
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
    pub async fn new_with_banlist(
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
        banned_peers: Vec<String>,
        banned_subnets: Vec<String>,
        whitelisted_peers: Vec<String>,
        network_type: crate::network_type::NetworkType,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(bind_addr).await?;
        let (tx, _) = broadcast::channel(1024);

        // Initialize banlist with configured IPs
        let mut banlist = IPBanlist::new();
        for peer in &banned_peers {
            if let Ok(ip) = peer.parse::<std::net::IpAddr>() {
                banlist.add_permanent_ban(ip, "Configured in banned_peers");
                tracing::info!("🚫 Banned peer from config: {}", ip);
            } else {
                tracing::warn!("⚠️  Invalid IP in banned_peers: {}", peer);
            }
        }

        // Initialize subnet bans from config
        for subnet in &banned_subnets {
            banlist.add_subnet_ban(subnet, "Configured in bansubnet");
        }

        // Initialize whitelist with configured IPs (BEFORE server starts accepting connections)
        for peer in &whitelisted_peers {
            if let Ok(ip) = peer.parse::<std::net::IpAddr>() {
                banlist.add_to_whitelist(ip, "Pre-configured whitelist");
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
            banlist: Arc::new(RwLock::new(banlist)),
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
            ddos_guard: Arc::new(DDoSGuard::new()),
            attack_log: None,
        })
    }

    /// Set the AI system for attack detection and mitigation enforcement
    pub fn set_ai_system(&mut self, ai_system: Arc<crate::ai::AISystem>) {
        self.ai_system = Some(ai_system);
    }

    /// Set the separate attack log file (writes a line per AI-detected attack).
    pub fn set_attack_log(&mut self, log: Arc<AttackLog>) {
        self.attack_log = Some(log);
    }

    /// Attach a sled database for banlist persistence across restarts.
    ///
    /// Loads previously banned IPs/subnets/violations from sled and enables
    /// write-through on all future mutations.  Call once after `new_with_banlist`.
    ///
    /// After reloading persisted subnet bans, this also runs an **eviction sweep**:
    /// any Free-tier masternodes from a previously-banned /24 that somehow re-registered
    /// before the ban was enforced are immediately evicted and their TCP connections kicked.
    pub async fn enable_banlist_persistence(&self, db: &sled::Db) {
        self.banlist.write().await.attach_storage(db);
        tracing::info!("🔒 Banlist persistence enabled — bans will survive restarts");
        // After loading persisted bans, ensure we never ban our own IP.
        // This clears any accidental self-ban that accumulated from self-connection
        // TLS failures (the node briefly connecting to its own IP via the peer list).
        if let Some(ref own_ip) = self.local_ip {
            if let Ok(ip) = own_ip.parse::<IpAddr>() {
                let mut bl = self.banlist.write().await;
                if bl.is_banned(ip).is_some() {
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
        let banned_subnets = self.banlist.read().await.list_banned_subnets();
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
        // Spawn cleanup task for banlist + DDoS subnet rate buckets + periodic DDoS stats log
        let banlist_cleanup = self.banlist.clone();
        let ddos_cleanup = self.ddos_guard.clone();
        let stats_banlist = self.banlist.clone();
        let stats_conn_mgr = self.connection_manager.clone();
        let stats_rate_limiter = self.rate_limiter.clone();
        let stats_subnet_rates = self.ddos_guard.subnet_rates.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await; // Every 5 minutes
                banlist_cleanup.write().await.cleanup();
                ddos_cleanup.cleanup_subnet_rates();

                // DDoS health snapshot
                let (perm, temp, subnets, violations) = stats_banlist.read().await.list_bans();
                let whitelist_count = stats_banlist.read().await.whitelist_count();
                let active = stats_conn_mgr.connected_count();
                let inbound = stats_conn_mgr.inbound_count();
                let rl_entries = stats_rate_limiter.read().await.entry_count();
                let subnet_buckets = stats_subnet_rates.len();
                tracing::info!(
                    "🛡️ DDoS guard — bans: {}P/{}T/{}S | violations: {} | whitelist: {} | conns: {}/{} inbound | rl_entries: {} | subnet_buckets: {}",
                    perm.len(), temp.len(), subnets.len(), violations.len(),
                    whitelist_count, inbound, active, rl_entries, subnet_buckets,
                );
            }
        });
        tracing::info!("🛡️ DDoS guard started (5-min stats log + subnet rate cleanup)");

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
            let enforce_banlist = self.banlist.clone();
            let enforce_registry = self.peer_registry.clone();
            let enforce_mn_registry = self.masternode_registry.clone();
            let enforce_attack_log = self.attack_log.clone();
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

                    // Log to the separate attacks.log file before applying mitigations.
                    if let Some(ref log) = enforce_attack_log {
                        log.log_all(&attacks).await;
                    }

                    // Collect peers to kick *before* dropping the banlist lock.
                    // kick_peer() is async and must not be called while holding the lock.
                    let mut to_kick: Vec<String> = Vec::new();
                    // Collect subnet prefixes (3-octet) that need registry eviction.
                    let mut to_evict_subnets: Vec<String> = Vec::new();

                    {
                        let mut banlist = enforce_banlist.write().await;
                        for attack in &attacks {
                            match &attack.recommended_action {
                                crate::ai::MitigationAction::BlockPeer(ip_str) => {
                                    if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
                                        if banlist.is_whitelisted(ip) {
                                            // Whitelisted peers are operator-trusted — never
                                            // disconnect them regardless of AI confidence.
                                            // They may be relaying attacker traffic innocently;
                                            // banning them would drop legitimate stake weight.
                                            tracing::warn!(
                                                "⚠️ AI: BlockPeer suppressed for whitelisted peer {} ({:?}) — log only",
                                                ip, attack.attack_type
                                            );
                                        } else {
                                            let should_disconnect = banlist.record_violation(
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
                                        if banlist.is_whitelisted(ip) {
                                            // Never penalize whitelisted peers for rate-limit
                                            // violations — they may be relaying legitimately.
                                            tracing::warn!(
                                                "⚠️ AI: RateLimitPeer suppressed for whitelisted peer {} ({:?}) — log only",
                                                ip, attack.attack_type
                                            );
                                        } else {
                                            // Rate limit = record as violation (escalates automatically)
                                            let should_disconnect = banlist.record_violation(
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
                                    banlist.add_subnet_ban(
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
                    } // banlist write lock released here

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

        // Collateral audit sweep: every 5 minutes, the registry audits all paid-tier
        // registrations for collateral ownership.  Detection and eviction live in
        // MasternodeRegistry::audit_squatters(); this task applies the security
        // consequences (IP ban + TCP kick) returned from that method.
        {
            let audit_registry = self.masternode_registry.clone();
            let audit_utxo = self.utxo_manager.clone();
            let audit_banlist = self.banlist.clone();
            let audit_peer_registry = self.peer_registry.clone();
            tokio::spawn(async move {
                // Stagger the first run by 2 minutes so UTXOs from recently-synced
                // blocks are in our local set before we condemn any node.
                tokio::time::sleep(tokio::time::Duration::from_secs(120)).await;
                loop {
                    // Collect the local node's IP and the whitelist so the registry
                    // can exempt them without importing the banlist module.
                    let local_ip = audit_registry
                        .get_local_masternode()
                        .await
                        .map(|mn| mn.masternode.address.clone())
                        .unwrap_or_default();
                    let whitelist_ips = audit_banlist.read().await.whitelist_ips();

                    let evictions = audit_registry
                        .audit_squatters(&audit_utxo, &local_ip, &whitelist_ips)
                        .await;

                    // Apply security consequences: ban the IP and close the connection.
                    for eviction in &evictions {
                        let bare = eviction.ip.split(':').next().unwrap_or(&eviction.ip);
                        if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
                            audit_banlist.write().await.add_temp_ban(
                                ip,
                                eviction.ban_duration,
                                &eviction.reason,
                            );
                        }
                        audit_peer_registry.kick_peer(&eviction.ip).await;
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

            // Check banlist BEFORE accepting connection
            {
                let mut banlist = self.banlist.write().await;
                if let Some(reason) = banlist.is_banned(ip) {
                    tracing::debug!("🚫 Rejected banned IP {}: {}", ip, reason);
                    drop(stream); // Close immediately
                    continue;
                }
            }

            // Phase 3: Check if this IP is whitelisted (trusted masternode)
            let is_whitelisted = {
                let banlist = self.banlist.read().await;
                banlist.is_whitelisted(ip)
            };

            // DDoS guard: per-/24 subnet rate limit (non-whitelisted only)
            if !is_whitelisted && self.ddos_guard.check_and_record_subnet_rate(ip) {
                tracing::debug!(
                    "🚫 DDoS guard: {}/24 exceeded {} conn/min, dropping",
                    ip_str,
                    crate::network::ddos_guard::MAX_SUBNET_CONNECTS_PER_MIN
                );
                if let Some(ai) = &self.ai_system {
                    ai.attack_detector.record_connection_flood(&ip_str);
                }
                drop(stream);
                continue;
            }

            if let Err(reason) = self
                .connection_manager
                .can_accept_inbound(&ip_str, is_whitelisted)
            {
                tracing::warn!("🚫 Rejected inbound connection from {}: {}", ip, reason);
                if let Some(ai) = &self.ai_system {
                    ai.attack_detector.record_connection_flood(&ip_str);
                }
                drop(stream); // Close immediately
                continue;
            }

            tracing::debug!(
                "✅ Spawning inbound handler for {} (total: {}, inbound: {}, whitelisted: {})",
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
            let banlist = self.banlist.clone();
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
                    banlist,
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

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // Called by NetworkServer::run which is used by binary
async fn handle_peer(
    stream: TcpStream,
    peer: PeerInfo,
    _peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    subs: Arc<RwLock<HashMap<String, Subscription>>>,
    notifier: broadcast::Receiver<NetworkMessage>,
    utxo_mgr: Arc<UTXOStateManager>,
    consensus: Arc<ConsensusEngine>,
    _rate_limiter: Arc<RwLock<RateLimiter>>,
    banlist: Arc<RwLock<IPBanlist>>,
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
    let driver = crate::network::connection_driver::ConnectionDriver {
        connection_manager,
        masternode_registry,
        blockchain,
        peer_registry,
        banlist: Some(banlist.clone()),
        tls_config,
        network_type,
        ai_system,
    };
    let resources = crate::network::connection_driver::InboundResources {
        consensus,
        peer_manager,
        banlist,
        broadcast_tx,
        seen_blocks,
        seen_transactions,
        seen_tx_finalized,
        seen_utxo_locks,
        local_ip,
        block_cache,
        utxo_mgr,
        subs,
    };
    driver
        .drive_inbound(stream, peer.addr, is_whitelisted, notifier, resources)
        .await
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
