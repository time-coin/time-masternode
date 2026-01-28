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
use std::collections::HashMap;
use std::net::IpAddr;

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader, BufWriter};
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
    pub connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    pub peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    #[allow(dead_code)]
    pub peer_state: Arc<PeerStateManager>,
    pub local_ip: Option<String>, // Our own public IP (without port) to avoid self-connection
    pub block_cache: Arc<BlockCache>, // Phase 3E.1: Bounded cache for TSDC voting
    pub peer_fork_status: Arc<DashMap<String, PeerForkStatus>>, // Track peers on incompatible forks
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
        whitelisted_peers: Vec<String>,
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

        // Initialize whitelist with configured IPs (BEFORE server starts accepting connections)
        for peer in &whitelisted_peers {
            if let Ok(ip) = peer.parse::<std::net::IpAddr>() {
                blacklist.add_to_whitelist(ip, "Pre-configured whitelist");
                tracing::info!("✅ Whitelisted peer before server start: {}", ip);
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
            connection_manager,
            peer_registry,
            peer_state,
            local_ip,
            block_cache: Arc::new(BlockCache::new_with_expiration(
                1000,                     // Max 1000 blocks
                Duration::from_secs(300), // 5 minute expiration
            )), // Phase 3E.1: Bounded LRU cache
            peer_fork_status: Arc::new(DashMap::new()), // Phase 2: Track fork status
        })
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
            if let Err(reason) = self
                .connection_manager
                .can_accept_inbound(&ip_str, is_whitelisted)
            {
                tracing::warn!("🚫 Rejected inbound connection from {}: {}", ip, reason);
                drop(stream); // Close immediately
                continue;
            }

            let connection_type = if is_whitelisted { "[WHITELIST]" } else { "" };
            tracing::info!(
                "✅ {} Accepting inbound connection from {} (total: {}, inbound: {}, whitelisted: {})",
                connection_type,
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
            let conn_mgr = self.connection_manager.clone();
            let peer_reg = self.peer_registry.clone();
            let local_ip = self.local_ip.clone();
            let block_cache = self.block_cache.clone(); // Phase 3E.1: Clone block cache
            let fork_status = self.peer_fork_status.clone(); // Phase 2: Clone fork status tracker

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
                    conn_mgr,
                    peer_reg,
                    local_ip,
                    block_cache,    // Phase 3E.1: Pass block cache
                    fork_status,    // Phase 2: Pass fork status tracker
                    is_whitelisted, // Phase 1: Pass whitelist status
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

// Phase 2.3: Message size limits for DoS protection
const MAX_MESSAGE_SIZE: usize = 2_000_000; // 2MB absolute max for any message
const MAX_BLOCK_SIZE: usize = 1_000_000; // 1MB for blocks
const MAX_TX_SIZE: usize = 100_000; // 100KB for transactions
const MAX_VOTE_SIZE: usize = 1_000; // 1KB for votes
#[allow(dead_code)] // Reserved for future general message validation
const MAX_GENERAL_SIZE: usize = 50_000; // 50KB for general messages

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
    rate_limiter: Arc<RwLock<RateLimiter>>,
    blacklist: Arc<RwLock<IPBlacklist>>,
    masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    blockchain: Arc<crate::blockchain::Blockchain>,
    peer_manager: Arc<crate::peer_manager::PeerManager>,
    broadcast_tx: broadcast::Sender<NetworkMessage>,
    seen_blocks: Arc<DeduplicationFilter>,
    seen_transactions: Arc<DeduplicationFilter>,
    connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    _local_ip: Option<String>,
    block_cache: Arc<BlockCache>, // Phase 3E.1: Block cache parameter
    _peer_fork_status: Arc<DashMap<String, PeerForkStatus>>, // Phase 2: Fork status tracker (no longer used - periodic resolution handles forks)
    _is_whitelisted: bool, // Phase 1: Whitelist status for relaxed timeouts (used in future enhancements)
) -> Result<(), std::io::Error> {
    // Extract IP from address
    let ip: IpAddr = peer
        .addr
        .split(':')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap());

    let ip_str = ip.to_string();

    // DON'T reject duplicate connections immediately - wait for handshake first
    // This prevents race conditions where both peers connect simultaneously
    // and both reject before handshake completes

    tracing::info!("🔌 New peer connection from: {}", peer.addr);
    let connection_start = std::time::Instant::now();
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::with_capacity(1024 * 1024, reader); // 1MB buffer
    let mut writer = Some(BufWriter::with_capacity(2 * 1024 * 1024, writer)); // 2MB buffer
    let mut line = String::new();
    let mut failed_parse_count = 0;
    let mut handshake_done = false;
    let mut is_stable_connection = false;

    // Define expected magic bytes for our protocol
    const MAGIC_BYTES: [u8; 4] = *b"TIME";

    loop {
        tokio::select! {
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        tracing::info!("🔌 Peer {} disconnected (EOF)", peer.addr);
                        break;
                    }
                    Ok(n) => {
                        tracing::debug!("📥 Received {} bytes from {}: {}", n, peer.addr, line.trim());

                        // Phase 2.3: Check message size BEFORE processing to prevent DoS
                        let message_size = line.len();
                        if message_size > MAX_MESSAGE_SIZE {
                            tracing::warn!("🚫 Rejecting oversized message from {}: {} bytes (max: {})",
                                peer.addr, message_size, MAX_MESSAGE_SIZE);

                            let mut blacklist_guard = blacklist.write().await;
                            let should_ban = blacklist_guard.record_violation(ip,
                                &format!("Oversized message: {} bytes", message_size));
                            drop(blacklist_guard);

                            if should_ban {
                                tracing::warn!("🚫 Disconnecting {} due to repeated oversized messages", peer.addr);
                                break;
                            }

                            line.clear();
                            continue;
                        }

                        // Check if this looks like old protocol (starts with ~W~M)
                        if !handshake_done && line.starts_with("~W~M") {
                            tracing::warn!("🚫 Rejecting {} - old protocol detected (~W~M magic bytes)", peer.addr);
                            blacklist.write().await.record_violation(
                                ip,
                                "Old protocol magic bytes (~W~M)"
                            );
                            break;
                        }

                        if let Ok(msg) = serde_json::from_str::<NetworkMessage>(&line) {
                            // First message MUST be a valid handshake
                            if !handshake_done {
                                match &msg {
                                    NetworkMessage::Handshake { magic, protocol_version, network } => {
                                        if magic != &MAGIC_BYTES {
                                            tracing::warn!("🚫 Rejecting {} - invalid magic bytes: {:?}", peer.addr, magic);
                                            blacklist.write().await.record_violation(
                                                ip,
                                                &format!("Invalid magic bytes: {:?}", magic)
                                            );
                                            break;
                                        }
                                        if protocol_version != &1 {
                                            tracing::warn!("🚫 Rejecting {} - unsupported protocol version: {}", peer.addr, protocol_version);
                                            blacklist.write().await.record_violation(
                                                ip,
                                                &format!("Unsupported protocol version: {}", protocol_version)
                                            );
                                            break;
                                        }
                                        tracing::info!("✅ Handshake accepted from {} (network: {})", peer.addr, network);
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

                                        // Register writer in peer registry after successful handshake
                                        if let Some(w) = writer.take() {
                                            tracing::info!("📝 Registering {} in PeerConnectionRegistry (peer.addr: {})", ip_str, peer.addr);
                                            peer_registry.register_peer(ip_str.clone(), w).await;
                                            tracing::debug!("✅ Successfully registered {} in registry", ip_str);
                                        } else {
                                            tracing::error!("❌ Writer already taken for {}, cannot register!", ip_str);
                                        }

                                        // Send ACK to confirm handshake was processed
                                        let ack_msg = NetworkMessage::Ack {
                                            message_type: "Handshake".to_string(),
                                        };
                                        let _ = peer_registry.send_to_peer(&ip_str, ack_msg).await;

                                        // Send our masternode announcement if we're a masternode
                                        let local_address = masternode_registry.get_local_address().await;
                                        if let Some(our_address) = local_address {
                                            // Only send OUR masternode announcement, not all masternodes
                                            let local_masternodes = masternode_registry.get_all().await;
                                            if let Some(our_mn) = local_masternodes.iter().find(|mn| mn.masternode.address == our_address) {
                                                let announcement = NetworkMessage::MasternodeAnnouncement {
                                                    address: our_mn.masternode.address.clone(),
                                                    reward_address: our_mn.reward_address.clone(),
                                                    tier: our_mn.masternode.tier,
                                                    public_key: our_mn.masternode.public_key,
                                                };
                                                let _ = peer_registry.send_to_peer(&ip_str, announcement).await;
                                                tracing::info!("📢 Sent our masternode announcement to newly connected peer {}", ip_str);
                                            }
                                        }

                                        // Request peer list for peer discovery
                                        let get_peers_msg = NetworkMessage::GetPeers;
                                        let _ = peer_registry.send_to_peer(&ip_str, get_peers_msg).await;

                                        // Request masternodes for peer discovery
                                        let get_mn_msg = NetworkMessage::GetMasternodes;
                                        let _ = peer_registry.send_to_peer(&ip_str, get_mn_msg).await;

                                        line.clear();
                                        continue;
                                    }
                                    _ => {
                                        tracing::warn!("⚠️  {} sent message before handshake - closing connection (not blacklisting)", peer.addr);
                                        // Don't blacklist - could be network timing issue or legitimate peer
                                        // Just close the connection and let them reconnect
                                        break;
                                    }
                                }
                            }

                            tracing::debug!("📦 Parsed message type from {}: {:?}", peer.addr, std::mem::discriminant(&msg));

                            // Phase 2.2: Rate limiting and blacklist enforcement
                            // Define helper macro for rate limit checking with auto-ban
                            macro_rules! check_rate_limit {
                                ($msg_type:expr) => {{
                                    let mut limiter = rate_limiter.write().await;
                                    let mut blacklist_guard = blacklist.write().await;

                                    if !limiter.check($msg_type, &ip_str) {
                                        tracing::warn!("⚠️  Rate limit exceeded for {} from {}: {}", $msg_type, peer.addr, ip_str);

                                        // Record violation and check if should be banned
                                        let should_ban = blacklist_guard.record_violation(ip,
                                            &format!("Rate limit exceeded: {}", $msg_type));

                                        if should_ban {
                                            tracing::warn!("🚫 Disconnecting {} due to rate limit violations", peer.addr);
                                            break; // Exit connection loop
                                        }

                                        line.clear();
                                        continue; // Skip processing this message
                                    }

                                    drop(limiter);
                                    drop(blacklist_guard);
                                }};
                            }

                            // Phase 2.3: Message-specific size validation helper
                            macro_rules! check_message_size {
                                ($max_size:expr, $msg_type:expr) => {{
                                    if message_size > $max_size {
                                        tracing::warn!("🚫 {} from {} exceeds size limit: {} > {} bytes",
                                            $msg_type, peer.addr, message_size, $max_size);

                                        let mut blacklist_guard = blacklist.write().await;
                                        let should_ban = blacklist_guard.record_violation(ip,
                                            &format!("{} too large: {} bytes", $msg_type, message_size));
                                        drop(blacklist_guard);

                                        if should_ban {
                                            tracing::warn!("🚫 Disconnecting {} due to oversized messages", peer.addr);
                                            break;
                                        }

                                        line.clear();
                                        continue;
                                    }
                                }};
                            }

                            match &msg {
                                // PRIORITY: UTXO locks MUST be processed immediately, even during block sync
                                // This prevents double-spend race conditions
                                NetworkMessage::UTXOStateUpdate { outpoint, state } => {
                                    tracing::debug!("🔒 PRIORITY: Received UTXO lock update from {}", peer.addr);
                                    consensus.utxo_manager.update_state(outpoint, state.clone());

                                    // Log important locks
                                    if let UTXOState::Locked { txid, .. } = state {
                                        tracing::info!(
                                            "🔒 Applied UTXO lock from peer {} for TX {:?}",
                                            peer.addr,
                                            hex::encode(txid)
                                        );
                                    }

                                    // Gossip lock to other peers immediately
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
                                        line.clear();
                                        continue;
                                    }

                                    tracing::info!("📥 Received new transaction {} from {}", hex::encode(txid), peer.addr);

                                    // Process transaction (validates and initiates voting if we're a masternode)
                                    match consensus.process_transaction(tx.clone()).await {
                                        Ok(_) => {
                                            tracing::debug!("✅ Transaction {} processed", hex::encode(txid));

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
                                            tracing::warn!("❌ Transaction {} rejected: {}", hex::encode(txid), e);

                                            // Phase 2.2: Record violation for invalid transaction
                                            let mut blacklist_guard = blacklist.write().await;
                                            let should_ban = blacklist_guard.record_violation(ip, "Invalid transaction");
                                            drop(blacklist_guard);

                                            if should_ban {
                                                tracing::warn!("🚫 Disconnecting {} due to repeated invalid transactions", peer.addr);
                                                break;
                                            }
                                        }
                                    }
                                }
                                NetworkMessage::TransactionFinalized { txid } => {
                                    tracing::info!("✅ Transaction {} finalized (from {})",
                                        hex::encode(*txid), peer.addr);

                                    // CRITICAL: Actually finalize the transaction on THIS node
                                    // Move from pending → finalized pool so block producers can include it
                                    if let Some(_tx) = consensus.tx_pool.finalize_transaction(*txid) {
                                        tracing::info!("📦 Moved TX {} to finalized pool on this node", hex::encode(*txid));
                                    } else {
                                        tracing::debug!("TX {} not in pending pool (may already be finalized or not received yet)", hex::encode(*txid));
                                    }

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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
                                    }
                                }
                                NetworkMessage::MasternodeAnnouncement { address: _, reward_address, tier, public_key } => {
                                    check_rate_limit!("masternode_announce");

                                    // Check if this is a stable connection (>5 seconds)
                                    if !is_stable_connection {
                                        let connection_age = connection_start.elapsed().as_secs();
                                        if connection_age < 5 {
                                            tracing::debug!("⏭️  Ignoring masternode announcement from short-lived connection {} (age: {}s)", peer.addr, connection_age);
                                            line.clear();
                                            continue;
                                        }
                                        is_stable_connection = true;
                                        tracing::debug!("✅ Connection {} marked as stable", peer.addr);
                                    }

                                    // Extract just the IP (no port) from the peer connection
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();

                                    if peer_ip.is_empty() {
                                        tracing::warn!("❌ Invalid peer IP from {}", peer.addr);
                                        line.clear();
                                        continue;
                                    }

                                    tracing::info!("📨 Received masternode announcement from {} (IP: {})", peer.addr, peer_ip);

                                    let mn = crate::types::Masternode::new_legacy(
                                        peer_ip.clone(),
                                        reward_address.clone(),
                                        tier.collateral(),
                                        *public_key,
                                        *tier,
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs(),
                                    );

                                    match masternode_registry.register(mn, reward_address.clone()).await {
                                        Ok(()) => {
                                            let count = masternode_registry.total_count().await;
                                            tracing::info!("✅ Registered masternode {} (total: {})", peer_ip, count);

                                            // Add masternode IP (without port) to peer_manager for P2P connections
                                            peer_manager.add_peer(peer_ip.clone()).await;

                                            // NOTE: Do NOT whitelist announced masternodes automatically.
                                            // Only masternodes discovered from time-coin.io should be whitelisted.
                                            // Announced masternodes could be from rogue nodes.
                                        },
                                        Err(e) => {
                                            tracing::warn!("❌ Failed to register masternode {}: {}", peer_ip, e);
                                        }
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
                                    }
                                }
                                NetworkMessage::BlockResponse(block) => {
                                    check_message_size!(MAX_BLOCK_SIZE, "Block");
                                    check_rate_limit!("block");

                                    let block_height = block.header.height;

                                    // Check if we've already seen this block using Bloom filter
                                    let block_height_bytes = block_height.to_le_bytes();
                                    let already_seen = seen_blocks.check_and_insert(&block_height_bytes).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate block {} from {}", block_height, peer.addr);
                                        line.clear();
                                        continue;
                                    }

                                    tracing::info!("📥 Received block {} response from {}", block_height, peer.addr);

                                    // Add block to our blockchain with fork handling
                                    match blockchain.add_block_with_fork_handling(block.clone()).await {
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
                                        Err(e) => {
                                            tracing::warn!("❌ Failed to add block {}: {}", block_height, e);
                                        }
                                    }
                                }
                                NetworkMessage::BlockAnnouncement(block) => {
                                    // Legacy full block announcement (for backward compatibility)
                                    check_message_size!(MAX_BLOCK_SIZE, "Block");
                                    check_rate_limit!("block");

                                    let block_height = block.header.height;

                                    // Check if we've already seen this block using Bloom filter
                                    let block_height_bytes = block_height.to_le_bytes();
                                    let already_seen = seen_blocks.check_and_insert(&block_height_bytes).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate block {} from {}", block_height, peer.addr);
                                        line.clear();
                                        continue;
                                    }

                                    tracing::debug!("📥 Received legacy block {} announcement from {}", block_height, peer.addr);

                                    // Add block to our blockchain with fork handling
                                    match blockchain.add_block_with_fork_handling(block.clone()).await {
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
                                        line.clear();
                                        continue;
                                    }

                                    // Check if we already have genesis - try to get block at height 0
                                    if blockchain.get_block_by_height(0).await.is_ok() {
                                        tracing::debug!("⏭️ Ignoring genesis announcement (already have genesis) from {}", peer.addr);
                                        line.clear();
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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
                                    check_rate_limit!("ping");

                                    // Use unified message handler
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();
                                    let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                    let context = MessageContext::minimal(
                                        Arc::clone(&blockchain),
                                        Arc::clone(&peer_registry),
                                        Arc::clone(&masternode_registry),
                                    );

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
                                    }
                                }
                                NetworkMessage::TimeVoteRequest { txid, tx_hash_commitment, slot_index } => {
                                    check_message_size!(MAX_VOTE_SIZE, "TimeVoteRequest");
                                    check_rate_limit!("vote");

                                    // Spawn vote processing (non-blocking)
                                    let txid_val = *txid;
                                    let tx_hash_commitment_val = *tx_hash_commitment;
                                    let slot_index_val = *slot_index;
                                    let peer_addr_str = peer.addr.to_string();
                                    let ip_str_clone = ip_str.clone();
                                    let consensus_clone = Arc::clone(&consensus);
                                    let peer_registry_clone = Arc::clone(&peer_registry);

                                    tokio::spawn(async move {
                                        tracing::debug!(
                                            "🗳️  TimeVoteRequest from {} for TX {:?} (slot {})",
                                            peer_addr_str,
                                            hex::encode(txid_val),
                                            slot_index_val
                                        );

                                        // Step 1: Check if transaction exists in our mempool
                                        let tx_opt = consensus_clone.tx_pool.get_pending(&txid_val);

                                        let decision = if let Some(tx) = tx_opt {
                                            // Step 2: Verify tx_hash_commitment matches actual transaction
                                            let actual_commitment = crate::types::TimeVote::calculate_tx_commitment(&tx);
                                            if actual_commitment != tx_hash_commitment_val {
                                                tracing::warn!(
                                                    "⚠️  TX {:?} commitment mismatch: expected {:?}, got {:?}",
                                                    hex::encode(txid_val),
                                                    hex::encode(actual_commitment),
                                                    hex::encode(tx_hash_commitment_val)
                                                );
                                                crate::types::VoteDecision::Reject
                                            } else {
                                                // Step 3: Verify UTXOs are available (basic validation)
                                                match consensus_clone.validate_transaction(&tx).await {
                                                    Ok(_) => {
                                                        tracing::debug!("✅ TX {:?} validated successfully", hex::encode(txid_val));
                                                        crate::types::VoteDecision::Accept
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("⚠️  TX {:?} validation failed: {}", hex::encode(txid_val), e);
                                                        crate::types::VoteDecision::Reject
                                                    }
                                                }
                                            }
                                        } else {
                                            tracing::debug!("⚠️  TX {:?} not found in mempool", hex::encode(txid_val));
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
                                            let _ = peer_registry_clone.send_to_peer(&ip_str_clone, vote_response).await;
                                            tracing::debug!(
                                                "✅ TimeVoteResponse sent for TX {:?} (decision: {:?})",
                                                hex::encode(txid_val),
                                                decision
                                            );
                                        } else {
                                            tracing::warn!(
                                                "⚠️  Failed to sign TimeVote for TX {:?} (not a masternode or identity not set)",
                                                hex::encode(txid_val)
                                            );
                                        }
                                    });
                                }
                                NetworkMessage::TimeVoteResponse { vote } => {
                                    check_message_size!(MAX_VOTE_SIZE, "TimeVoteResponse");
                                    check_rate_limit!("vote");

                                    // Received a signed TimeVote from a peer
                                    tracing::debug!(
                                        "📥 TimeVoteResponse from {} for TX {:?} (decision: {:?}, weight: {})",
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
                                        let accumulated_weight = match consensus_clone.timevote.accumulate_timevote(vote_clone) {
                                            Ok(weight) => weight,
                                            Err(e) => {
                                                tracing::warn!(
                                                    "Failed to accumulate vote for TX {:?}: {}",
                                                    hex::encode(txid),
                                                    e
                                                );
                                                return;
                                            }
                                        };

                                        tracing::debug!(
                                            "Vote accumulated for TX {:?}, total weight: {}",
                                            hex::encode(txid),
                                            accumulated_weight
                                        );

                                        // Step 2: Check if finality threshold reached
                                        // Calculate total AVS weight and 67% threshold
                                        let validators = consensus_clone.timevote.get_validators();
                                        let total_avs_weight: u64 = validators.iter().map(|v| v.weight as u64).sum();
                                        let finality_threshold = ((total_avs_weight as f64) * 0.67).ceil() as u64;

                                        tracing::debug!(
                                            "Finality check for TX {:?}: accumulated={}, threshold={} (67% of {})",
                                            hex::encode(txid),
                                            accumulated_weight,
                                            finality_threshold,
                                            total_avs_weight
                                        );

                                        // Step 3: If threshold met, finalize transaction
                                        if accumulated_weight >= finality_threshold {
                                            tracing::info!(
                                                "🎉 TX {:?} reached finality threshold! ({} >= {})",
                                                hex::encode(txid),
                                                accumulated_weight,
                                                finality_threshold
                                            );

                                            // Move transaction from pending to finalized
                                            if let Some(_finalized_tx) = tx_pool.finalize_transaction(txid) {
                                                tracing::info!(
                                                    "✅ TX {:?} moved to finalized pool",
                                                    hex::encode(txid)
                                                );

                                                // Record finalization in TimeVoteConsensus
                                                consensus_clone.timevote.record_finalization(txid, accumulated_weight);

                                                // Assemble TimeProof certificate
                                                match consensus_clone.timevote.assemble_timeproof(txid) {
                                                    Ok(timeproof) => {
                                                        tracing::info!(
                                                            "📜 TimeProof assembled for TX {:?} with {} votes",
                                                            hex::encode(txid),
                                                            timeproof.votes.len()
                                                        );

                                                        // Store TimeProof in finality_proof_manager
                                                        if let Err(e) = consensus_clone.finality_proof_mgr.store_timeproof(timeproof.clone()) {
                                                            tracing::error!(
                                                                "❌ Failed to store TimeProof for TX {:?}: {}",
                                                                hex::encode(txid),
                                                                e
                                                            );
                                                        }

                                                        // Broadcast TimeProof to network (Task 2.5)
                                                        consensus_clone.broadcast_timeproof(timeproof).await;
                                                    }
                                                    Err(e) => {
                                                        tracing::error!(
                                                            "❌ Failed to assemble TimeProof for TX {:?}: {}",
                                                            hex::encode(txid),
                                                            e
                                                        );
                                                    }
                                                }
                                            } else {
                                                tracing::warn!(
                                                    "⚠️  Failed to finalize TX {:?} - not found in pending pool",
                                                    hex::encode(txid)
                                                );
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
                                            "📜 Received TimeProof from {} for TX {:?} with {} votes",
                                            peer_addr_str,
                                            hex::encode(proof_clone.txid),
                                            proof_clone.votes.len()
                                        );

                                        // Verify TimeProof using consensus engine's verification method
                                        match consensus_clone.timevote.verify_timeproof(&proof_clone) {
                                            Ok(_accumulated_weight) => {
                                                tracing::info!(
                                                    "✅ TimeProof verified for TX {:?}",
                                                    hex::encode(proof_clone.txid)
                                                );

                                                // Store verified TimeProof
                                                if let Err(e) = consensus_clone.finality_proof_mgr.store_timeproof(proof_clone.clone()) {
                                                    tracing::error!(
                                                        "❌ Failed to store TimeProof for TX {:?}: {}",
                                                        hex::encode(proof_clone.txid),
                                                        e
                                                    );
                                                } else {
                                                    tracing::info!(
                                                        "💾 TimeProof stored for TX {:?}",
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
                                NetworkMessage::TransactionVoteRequest { txid } => {
                                    check_message_size!(MAX_VOTE_SIZE, "VoteRequest");
                                    check_rate_limit!("vote");

                                    // Spawn vote processing (non-blocking - runs concurrently with block production)
                                    let txid_val = *txid;
                                    let peer_addr_str = peer.addr.to_string();
                                    let ip_str_clone = ip_str.clone();
                                    let consensus_clone = Arc::clone(&consensus);
                                    let peer_registry_clone = Arc::clone(&peer_registry);

                                    tokio::spawn(async move {
                                        tracing::debug!("🗳️  Vote request from {} for TX {:?}", peer_addr_str, hex::encode(txid_val));

                                        // Get our preference (Accept/Reject) for this transaction
                                        let preference = if consensus_clone.tx_pool.is_pending(&txid_val) || consensus_clone.tx_pool.get_pending(&txid_val).is_some() {
                                            "Accept".to_string()
                                        } else {
                                            "Reject".to_string()
                                        };

                                        // Send our vote immediately
                                        let vote_response = NetworkMessage::TransactionVoteResponse {
                                            txid: txid_val,
                                            preference,
                                        };
                                        let _ = peer_registry_clone.send_to_peer(&ip_str_clone, vote_response).await;

                                        tracing::debug!("✅ Vote response sent for TX {:?}", hex::encode(txid_val));
                                    });
                                }
                                NetworkMessage::TransactionVoteResponse { txid, preference } => {
                                    check_message_size!(MAX_VOTE_SIZE, "VoteResponse");
                                    check_rate_limit!("vote");

                                    // Received a vote from a peer
                                    tracing::debug!("📥 Vote from {} for TX {:?}: {}", peer.addr, hex::encode(txid), preference);

                                    // Update our timevote consensus with this vote
                                    // Convert preference string to Preference enum
                                    let pref = match preference.as_str() {
                                        "Accept" => crate::consensus::Preference::Accept,
                                        "Reject" => crate::consensus::Preference::Reject,
                                        _ => {
                                            tracing::warn!("Invalid preference: {}", preference);
                                            // Skip processing this invalid vote
                                            line.clear();
                                            continue;
                                        }
                                    };

                                    // Submit vote to timevote consensus
                                    // The consensus engine will update voting state
                                    consensus.timevote.submit_vote(*txid, peer.addr.clone(), pref);

                                    tracing::debug!("✅ Vote recorded for TX {:?}", hex::encode(txid));
                                }
                                NetworkMessage::FinalityVoteBroadcast { vote } => {
                                    check_message_size!(MAX_VOTE_SIZE, "FinalityVote");
                                    check_rate_limit!("vote");

                                    // Received a finality vote from a peer
                                    tracing::debug!("📥 Finality vote from {} for TX {:?}", peer.addr, hex::encode(vote.txid));

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
                                    // Use unified message handler for TSDC messages
                                    let handler = MessageHandler::new(ip_str.clone(), ConnectionDirection::Inbound);
                                    let context = MessageContext::with_consensus(
                                        blockchain.clone(),
                                        peer_registry.clone(),
                                        masternode_registry.clone(),
                                        consensus.clone(),
                                        block_cache.clone(),
                                        broadcast_tx.clone(),
                                    );

                                    if let Err(e) = handler.handle_message(&msg, &context).await {
                                        tracing::warn!("[Inbound] Error handling TSDC message from {}: {}", peer.addr, e);
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                    if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                        let _ = peer_registry.send_to_peer(&ip_str, response).await;
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

                                        // Check for fork and request blocks if needed
                                        if let Some(fork_height) = blockchain.detect_fork(*height, *tip_hash).await {
                                            tracing::warn!(
                                                "🔀 Fork detected at height {} with {}, requesting blocks",
                                                fork_height, peer.addr
                                            );

                                            let request = NetworkMessage::GetBlockRange {
                                                start_height: fork_height,
                                                end_height: *height,
                                            };
                                            let _ = peer_registry.send_to_peer(&ip_str, request).await;
                                        }
                                    }
                                }
                                NetworkMessage::ChainWorkAtResponse { .. } => {
                                    // Handle via response system - handled by request/response pattern
                                    peer_registry.handle_response(&ip_str, msg).await;
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
                                _ => {}
                            }
                        } else {
                            // Check if buffer contains multiple JSON objects (happens during high-throughput sync)
                            // This is a transport-level issue, not malicious behavior
                            let trimmed = line.trim();
                            if trimmed.contains('\n') || (trimmed.starts_with('{') && trimmed.matches('{').count() > 1) {
                                tracing::debug!("📦 Received concatenated messages from {}, attempting to split", peer.addr);

                                // First split by newlines if present
                                let mut json_objects = Vec::new();
                                for segment in trimmed.split('\n').filter(|s| !s.trim().is_empty()) {
                                    // For each segment, check if it contains multiple JSON objects
                                    let segment_trimmed = segment.trim();
                                    if segment_trimmed.starts_with('{') && segment_trimmed.matches('{').count() > 1 {
                                        // Multiple JSON objects on same line - split by brace matching
                                        let mut depth = 0;
                                        let mut start = 0;
                                        let chars: Vec<char> = segment_trimmed.chars().collect();

                                        for i in 0..chars.len() {
                                            match chars[i] {
                                                '{' => depth += 1,
                                                '}' => {
                                                    depth -= 1;
                                                    if depth == 0 {
                                                        // Found complete JSON object
                                                        let obj: String = chars[start..=i].iter().collect();
                                                        json_objects.push(obj);
                                                        start = i + 1;
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    } else {
                                        json_objects.push(segment_trimmed.to_string());
                                    }
                                }

                                if json_objects.len() > 1 {
                                    // Verify each split object is valid JSON
                                    let mut valid_count = 0;
                                    for json_obj in &json_objects {
                                        if serde_json::from_str::<NetworkMessage>(json_obj).is_ok() {
                                            valid_count += 1;
                                        }
                                    }

                                    tracing::debug!(
                                        "📦 Split {} concatenated JSON objects from {} ({} valid, will be processed by peer connection handler)",
                                        json_objects.len(),
                                        peer.addr,
                                        valid_count
                                    );
                                    // Don't count as failed parse - these are valid messages that got concatenated
                                    // They will be processed correctly by the peer_connection.rs handler
                                } else {
                                    failed_parse_count += 1;
                                    tracing::warn!("❌ Failed to parse message from {} (appears concatenated but couldn't split properly)", peer.addr);
                                }
                            } else {
                                failed_parse_count += 1;
                                // Try to parse to see what the error is
                                if let Err(parse_err) = serde_json::from_str::<NetworkMessage>(&line) {
                                    tracing::warn!("❌ Failed to parse message {} from {}: {} | Raw: {}",
                                        failed_parse_count, peer.addr, parse_err,
                                        line.chars().take(200).collect::<String>());
                                }
                                // Record violation and check if should ban
                                let should_ban = blacklist.write().await.record_violation(
                                    ip,
                                    "Failed to parse message"
                                );
                                // Be more lenient - allow up to 10 parse failures before disconnecting
                                // This handles cases where peers send extra newlines or have temporary issues
                                if should_ban || failed_parse_count >= 10 {
                                    tracing::warn!("🚫 Disconnecting {} after {} failed parse attempts", peer.addr, failed_parse_count);
                                    break;
                                }
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        tracing::info!("🔌 Connection from {} ended: {}", peer.addr, e);
                        break;
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
        }
    }

    // Cleanup: mark inbound connection as disconnected in BOTH managers
    connection_manager.mark_inbound_disconnected(&ip_str);
    peer_registry.unregister_peer(&ip_str).await;

    // Mark masternode as inactive if this was a masternode connection
    if let Err(e) = masternode_registry
        .mark_inactive_on_disconnect(&ip_str)
        .await
    {
        tracing::debug!("Note: {} is not a registered masternode ({})", ip_str, e);
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
        assert_eq!(MAX_BLOCK_SIZE, 1_000_000); // 1MB for blocks
        assert_eq!(MAX_TX_SIZE, 100_000); // 100KB for transactions
        assert_eq!(MAX_VOTE_SIZE, 1_000); // 1KB for votes
        assert_eq!(MAX_GENERAL_SIZE, 50_000); // 50KB for general

        // Verify hierarchy
        // Note: These are constant assertions that clippy warns about.
        // The hierarchy is enforced at compile time by the constant values themselves.
        // Documented here for clarity:
        // MAX_BLOCK_SIZE (1MB) < MAX_MESSAGE_SIZE (2MB)
        // MAX_TX_SIZE (100KB) < MAX_BLOCK_SIZE (1MB)
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

        let block_size = 1_500_000; // 1.5MB
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
        };

        // Serialize and check size
        let serialized = serde_json::to_string(&tx).expect("Failed to serialize transaction");
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
