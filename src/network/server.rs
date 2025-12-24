use crate::block::types::Block;
use crate::consensus::ConsensusEngine;
use crate::network::blacklist::IPBlacklist;
use crate::network::dedup_filter::DeduplicationFilter;
use crate::network::message::{NetworkMessage, Subscription, UTXOStateChange};
use crate::network::peer_connection::PeerStateManager;
use crate::network::rate_limiter::RateLimiter;
use crate::types::{Hash256, Masternode, OutPoint};
use crate::utxo_manager::UTXOStateManager;
use dashmap::DashMap;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

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
    pub block_cache: Arc<DashMap<Hash256, Block>>, // Phase 3E.1: Cache blocks during voting
}

pub struct PeerInfo {
    pub addr: String,
    #[allow(dead_code)]
    pub is_masternode: bool,
}

impl NetworkServer {
    #[allow(clippy::too_many_arguments)]
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
        let listener = TcpListener::bind(bind_addr).await?;
        let (tx, _) = broadcast::channel(1024);

        Ok(Self {
            listener,
            peers: Arc::new(RwLock::new(HashMap::new())),
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            tx_notifier: tx,
            utxo_manager,
            consensus,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new())),
            blacklist: Arc::new(RwLock::new(IPBlacklist::new())),
            masternode_registry: masternode_registry.clone(),
            blockchain,
            peer_manager,
            seen_blocks: Arc::new(DeduplicationFilter::new(Duration::from_secs(300))), // 5 min rotation
            seen_transactions: Arc::new(DeduplicationFilter::new(Duration::from_secs(600))), // 10 min rotation
            connection_manager,
            peer_registry,
            peer_state,
            local_ip,
            block_cache: Arc::new(DashMap::new()), // Phase 3E.1: Initialize block cache
        })
    }

    pub async fn run(&mut self) -> Result<(), std::io::Error> {
        // Spawn cleanup task for blacklist
        let blacklist_cleanup = self.blacklist.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(300)).await; // Every 5 minutes
                blacklist_cleanup.write().await.cleanup();
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

            // Check blacklist BEFORE accepting connection
            {
                let mut blacklist = self.blacklist.write().await;
                if let Some(reason) = blacklist.is_blacklisted(ip) {
                    tracing::debug!("🚫 Rejected blacklisted IP {}: {}", ip, reason);
                    drop(stream); // Close immediately
                    continue;
                }
            }

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
                    block_cache, // Phase 3E.1: Pass block cache
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

#[allow(clippy::too_many_arguments)]
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
    block_cache: Arc<DashMap<Hash256, Block>>, // Phase 3E.1: Block cache parameter
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
    let mut reader = BufReader::new(reader);
    let mut writer = Some(BufWriter::new(writer));
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

                                        // NOW check for duplicate connections after handshake
                                        // This prevents race conditions where both peers connect simultaneously
                                        let has_outbound = connection_manager.is_connected(&ip_str);

                                        if has_outbound {
                                            // We have an outbound connection to this peer
                                            // Use deterministic tie-breaking based on IP comparison
                                            let should_we_connect = connection_manager.should_connect_to(&ip_str);

                                            if should_we_connect {
                                                // Our IP is higher, we should be the one connecting OUT
                                                // So reject this INbound connection
                                                tracing::debug!(
                                                    "🔄 Rejecting duplicate inbound from {} after handshake (we should connect OUT to them)",
                                                    peer.addr
                                                );
                                                // Send ACK first so client doesn't get "connection reset"
                                                let ack_msg = NetworkMessage::Ack {
                                                    message_type: "Handshake".to_string(),
                                                };
                                                if let Some(w) = writer.take() {
                                                    peer_registry.register_peer(ip_str.clone(), w).await;
                                                    let _ = peer_registry.send_to_peer(&ip_str, ack_msg).await;
                                                }
                                                break; // Close connection gracefully
                                            }
                                            // Otherwise, accept this inbound and close the outbound
                                            tracing::debug!(
                                                "✅ Accepting inbound from {} (they should connect OUT, closing our outbound)",
                                                peer.addr
                                            );
                                            // Close the outbound connection in favor of this inbound
                                            connection_manager.remove(&ip_str);
                                        }

                                        // Mark this inbound connection
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
                                        let local_masternodes = masternode_registry.get_all().await;
                                        for mn_info in local_masternodes {
                                            let announcement = NetworkMessage::MasternodeAnnouncement {
                                                address: mn_info.masternode.address.clone(),
                                                reward_address: mn_info.reward_address.clone(),
                                                tier: mn_info.masternode.tier.clone(),
                                                public_key: mn_info.masternode.public_key,
                                            };
                                            let _ = peer_registry.send_to_peer(&ip_str, announcement).await;
                                            tracing::info!("📢 Sent masternode announcement to newly connected peer {}", ip_str);
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
                            // ip_str already defined at top of function from peer IP extraction
                            let mut limiter = rate_limiter.write().await;

                            match &msg {
                                NetworkMessage::Ack { message_type } => {
                                    tracing::debug!("✅ Received ACK for {} from {}", message_type, peer.addr);
                                    // ACKs are informational, no action needed
                                }
                                NetworkMessage::TransactionBroadcast(tx) => {
                                    if limiter.check("tx", &ip_str) {
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
                                            }
                                        }
                                    }
                                }
                                NetworkMessage::TransactionFinalized { txid } => {
                                    tracing::info!("✅ Transaction {} finalized (from {})",
                                        hex::encode(*txid), peer.addr);

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
                                NetworkMessage::UTXOStateQuery(outpoints) => {
                                    if limiter.check("utxo_query", &ip_str) {
                                        let mut responses = Vec::new();
                                        for op in outpoints {
                                            if let Some(state) = utxo_mgr.get_state(op) {
                                                responses.push((op.clone(), state));
                                            }
                                        }
                                        let reply = NetworkMessage::UTXOStateResponse(responses);
                                        let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                    }
                                }
                                NetworkMessage::Subscribe(sub) => {
                                    if limiter.check("subscribe", &ip_str) {
                                        subs.write().await.insert(sub.id.clone(), sub.clone());
                                    }
                                }
                                NetworkMessage::GetBlockHeight => {
                                    let height = blockchain.get_height().await;
                                    tracing::debug!("📥 Received GetBlockHeight from {}, responding with height {}", peer.addr, height);
                                    let reply = NetworkMessage::BlockHeightResponse(height);
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                }
                                NetworkMessage::GetPendingTransactions => {
                                    // Get pending transactions from mempool
                                    let pending_txs = blockchain.get_pending_transactions();
                                    let reply = NetworkMessage::PendingTransactionsResponse(pending_txs);
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                }
                                NetworkMessage::GetBlocks(start, end) => {
                                    let mut blocks = Vec::new();
                                    for h in *start..=(*end).min(start + 100) {
                                        if let Ok(block) = blockchain.get_block_by_height(h).await {
                                            blocks.push(block);
                                        }
                                    }
                                    let reply = NetworkMessage::BlocksResponse(blocks);
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                }
                                NetworkMessage::GetUTXOStateHash => {
                                    let height = blockchain.get_height().await;
                                    let utxo_hash = blockchain.get_utxo_state_hash().await;
                                    let utxo_count = blockchain.get_utxo_count().await;

                                    let reply = NetworkMessage::UTXOStateHashResponse {
                                        hash: utxo_hash,
                                        height,
                                        utxo_count,
                                    };
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                    tracing::debug!("📤 Sent UTXO state hash to {}", peer.addr);
                                }
                                NetworkMessage::GetUTXOSet => {
                                    let utxos = blockchain.get_all_utxos().await;
                                    let reply = NetworkMessage::UTXOSetResponse(utxos);
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                    tracing::info!("📤 Sent complete UTXO set to {}", peer.addr);
                                }
                                NetworkMessage::MasternodeAnnouncement { address: _, reward_address, tier, public_key } => {
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

                                    let mn = crate::types::Masternode {
                                        address: peer_ip.clone(), // Store only IP
                                        wallet_address: reward_address.clone(),
                                        collateral: tier.collateral(),
                                        tier: tier.clone(),
                                        public_key: *public_key,
                                        registered_at: std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs(),
                                    };

                                    match masternode_registry.register(mn, reward_address.clone()).await {
                                        Ok(()) => {
                                            let count = masternode_registry.total_count().await;
                                            tracing::info!("✅ Registered masternode {} (total: {})", peer_ip, count);

                                            // Add masternode IP (without port) to peer_manager for P2P connections
                                            peer_manager.add_peer(peer_ip).await;
                                        },
                                        Err(e) => {
                                            tracing::warn!("❌ Failed to register masternode {}: {}", peer_ip, e);
                                        }
                                    }
                                }
                                NetworkMessage::GetPeers => {
                                    tracing::debug!("📥 Received GetPeers request from {}", peer.addr);
                                    let peers = peer_manager.get_all_peers().await;
                                    let response = NetworkMessage::PeersResponse(peers.clone());
                                    let _ = peer_registry.send_to_peer(&ip_str, response).await;
                                    tracing::debug!("📤 Sent {} peer(s) to {}", peers.len(), peer.addr);
                                }
                                NetworkMessage::GetMasternodes => {
                                    tracing::info!("📥 Received GetMasternodes request from {}", peer.addr);
                                    let all_masternodes = masternode_registry.list_all().await;
                                    let mn_data: Vec<crate::network::message::MasternodeAnnouncementData> = all_masternodes
                                        .iter()
                                        .map(|mn_info| {
                                            // Strip port from address to ensure consistency
                                            let ip_only = mn_info.masternode.address.split(':').next()
                                                .unwrap_or(&mn_info.masternode.address).to_string();
                                            crate::network::message::MasternodeAnnouncementData {
                                                address: ip_only,
                                                reward_address: mn_info.reward_address.clone(),
                                                tier: mn_info.masternode.tier.clone(),
                                                public_key: mn_info.masternode.public_key,
                                            }
                                        })
                                        .collect();

                                    let response = NetworkMessage::MasternodesResponse(mn_data);
                                    let _ = peer_registry.send_to_peer(&ip_str, response).await;
                                    tracing::info!("📤 Responded with {} masternode(s) to {}", all_masternodes.len(), peer.addr);
                                }
                                NetworkMessage::PeersResponse(peers) => {
                                    tracing::debug!("📥 Received PeersResponse from {} with {} peer(s)", peer.addr, peers.len());
                                    let mut added = 0;
                                    for peer_addr in peers {
                                        if peer_manager.add_peer_candidate(peer_addr.clone()).await {
                                            added += 1;
                                        }
                                    }
                                    if added > 0 {
                                        tracing::info!("✓ Added {} new peer candidate(s) from {}", added, peer.addr);
                                    }
                                }
                                NetworkMessage::MasternodesResponse(masternodes) => {
                                    tracing::debug!("📥 Received MasternodesResponse from {} with {} masternode(s)", peer.addr, masternodes.len());
                                    let mut registered = 0;
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    for mn_data in masternodes {
                                        // Reconstruct Masternode object from response data
                                        let masternode = Masternode {
                                            address: mn_data.address.clone(),
                                            wallet_address: mn_data.reward_address.clone(),
                                            tier: mn_data.tier.clone(),
                                            public_key: mn_data.public_key,
                                            collateral: 0, // Collateral unknown from peer response
                                            registered_at: now,
                                        };
                                        // Register each masternode from the response
                                        if masternode_registry.register(masternode, mn_data.reward_address.clone()).await.is_ok() {
                                            registered += 1;
                                        }
                                    }
                                    if registered > 0 {
                                        tracing::info!("✓ Registered {} masternode(s) from peer exchange with {}", registered, peer.addr);
                                    }
                                }
                                NetworkMessage::BlockAnnouncement(block) => {
                                    let block_height = block.header.height;

                                    // Check if we've already seen this block using Bloom filter
                                    let block_height_bytes = block_height.to_le_bytes();
                                    let already_seen = seen_blocks.check_and_insert(&block_height_bytes).await;

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate block {} from {}", block_height, peer.addr);
                                        line.clear();
                                        continue;
                                    }

                                    tracing::info!("📥 Received block {} announcement from {}", block_height, peer.addr);

                                    // Add block to our blockchain with fork handling
                                    match blockchain.add_block_with_fork_handling(block.clone()).await {
                                        Ok(true) => {
                                            tracing::info!("✅ Added block {} from {}", block_height, peer.addr);

                                            // GOSSIP: Relay to all other connected peers
                                            let msg = NetworkMessage::BlockAnnouncement(block.clone());
                                            match broadcast_tx.send(msg) {
                                                Ok(receivers) => {
                                                    tracing::info!("🔄 Gossiped block {} to {} other peer(s)", block_height, receivers.saturating_sub(1));
                                                }
                                                Err(e) => {
                                                    tracing::warn!("Failed to gossip block: {}", e);
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
                                NetworkMessage::GetBlockHash(height) => {
                                    tracing::debug!("📥 Received GetBlockHash({}) from {}", height, peer.addr);
                                    let hash = blockchain.get_block_hash_at_height(*height).await;
                                    let reply = NetworkMessage::BlockHashResponse {
                                        height: *height,
                                        hash,
                                    };
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                }
                                NetworkMessage::ConsensusQuery { height, block_hash } => {
                                    tracing::debug!("📥 Received ConsensusQuery for height {} from {}", height, peer.addr);
                                    let (agrees, our_hash) = blockchain.check_consensus_with_peer(*height, *block_hash).await;
                                    let reply = NetworkMessage::ConsensusQueryResponse {
                                        agrees,
                                        height: *height,
                                        their_hash: our_hash.unwrap_or([0u8; 32]),
                                    };
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                }
                                NetworkMessage::GetBlockRange { start_height, end_height } => {
                                    tracing::debug!("📥 Received GetBlockRange({}-{}) from {}", start_height, end_height, peer.addr);
                                    let blocks = blockchain.get_block_range(*start_height, *end_height).await;
                                    let reply = NetworkMessage::BlockRangeResponse(blocks);
                                    let _ = peer_registry.send_to_peer(&ip_str, reply).await;
                                    tracing::debug!("📤 Sent block range to {}", peer.addr);
                                }
                                NetworkMessage::BlocksResponse(blocks) | NetworkMessage::BlockRangeResponse(blocks) => {
                                    // Handle block sync response
                                    let block_count = blocks.len();
                                    if block_count == 0 {
                                        tracing::debug!("📥 Received empty blocks response from {}", peer.addr);
                                    } else {
                                        let start_height = blocks.first().map(|b| b.header.height).unwrap_or(0);
                                        let end_height = blocks.last().map(|b| b.header.height).unwrap_or(0);
                                        let our_height = blockchain.get_height().await;

                                        tracing::info!("📥 Received {} blocks (height {}-{}) from {} (our height: {})",
                                            block_count, start_height, end_height, peer.addr, our_height);

                                        // Check if this is from a different chain (fork detection)
                                        if start_height <= our_height && start_height > 0 {
                                            // Peer is sending blocks we might already have
                                            // Check if the first block matches what we have
                                            if let Ok(our_block) = blockchain.get_block_by_height(start_height).await {
                                                let incoming_hash = blocks.first().unwrap().hash();
                                                let our_hash = our_block.hash();

                                                if incoming_hash != our_hash {
                                                    tracing::warn!(
                                                        "🔀 Fork detected at height {}: peer has different block",
                                                        start_height
                                                    );

                                                    // Check if peer's chain is longer
                                                    if end_height > our_height {
                                                        // Find common ancestor by checking parent hashes
                                                        let common_ancestor = start_height - 1;

                                                        // Simple approach: rollback to before the fork
                                                        // and apply the new blocks
                                                        tracing::info!(
                                                            "🔄 Peer has longer chain ({} vs {}), reorganizing from height {}",
                                                            end_height, our_height, common_ancestor
                                                        );

                                                        // Verify we have the common ancestor
                                                        if common_ancestor > 0 {
                                                            if let Ok(first_block) = blocks.first().ok_or("no blocks") {
                                                                if let Ok(our_prev) = blockchain.get_block_hash_at_height(common_ancestor).await.ok_or("no prev") {
                                                                    if first_block.header.previous_hash == our_prev {
                                                                        // Common ancestor confirmed, do reorg
                                                                        match blockchain.reorganize_to_chain(common_ancestor, blocks.clone()).await {
                                                                            Ok(()) => {
                                                                                tracing::info!("✅ Chain reorganization successful");
                                                                                continue;
                                                                            }
                                                                            Err(e) => {
                                                                                tracing::error!("❌ Chain reorganization failed: {}", e);
                                                                                continue;
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }

                                                        // Couldn't verify common ancestor, skip these blocks
                                                        tracing::warn!("⚠️ Could not verify common ancestor, skipping fork");
                                                        continue;
                                                    } else {
                                                        // Our chain is same length or longer, keep it
                                                        tracing::info!("📊 Keeping our chain (same or longer)");
                                                        continue;
                                                    }
                                                }
                                            }
                                        }

                                        // Normal case: apply blocks sequentially
                                        let mut added = 0;
                                        let mut skipped = 0;
                                        for block in blocks {
                                            match blockchain.add_block_with_fork_handling(block.clone()).await {
                                                Ok(true) => {
                                                    added += 1;
                                                }
                                                Ok(false) => {
                                                    skipped += 1;
                                                }
                                                Err(e) => {
                                                    // Could be duplicate or invalid - log at debug level
                                                    tracing::debug!("⏭️ Skipped block {}: {}", block.header.height, e);
                                                    skipped += 1;
                                                }
                                            }
                                        }
                                        if added > 0 {
                                            tracing::info!("✅ Synced {} blocks from {} (skipped {})", added, peer.addr, skipped);
                                        }
                                    }
                                }
                                // Heartbeat Messages
                                NetworkMessage::HeartbeatBroadcast(heartbeat) => {
                                    tracing::debug!("💓 Received heartbeat from {} seq {}",
                                        heartbeat.masternode_address, heartbeat.sequence_number);

                                    // Process heartbeat through masternode registry
                                    if let Err(e) = masternode_registry.receive_heartbeat_broadcast(heartbeat.clone()).await {
                                        tracing::warn!("Failed to process heartbeat: {}", e);
                                    }

                                    // Re-broadcast to other peers (gossip propagation)
                                    let msg = NetworkMessage::HeartbeatBroadcast(heartbeat.clone());
                                    let _ = masternode_registry.broadcast_message(msg).await;
                                }
                                NetworkMessage::HeartbeatAttestation(attestation) => {
                                    tracing::debug!("✍️ Received heartbeat attestation from {}",
                                        attestation.witness_address);

                                    // Process attestation through masternode registry
                                    if let Err(e) = masternode_registry.receive_attestation_broadcast(attestation.clone()).await {
                                        tracing::warn!("Failed to process attestation: {}", e);
                                    }

                                    // Re-broadcast to other peers (gossip propagation)
                                    let msg = NetworkMessage::HeartbeatAttestation(attestation.clone());
                                    let _ = masternode_registry.broadcast_message(msg).await;
                                }
                                // Health Check Messages
                                NetworkMessage::Ping { nonce, timestamp: _ } => {
                                    // Respond to ping with pong
                                    let pong_msg = NetworkMessage::Pong {
                                        nonce: *nonce,
                                        timestamp: chrono::Utc::now().timestamp(),
                                    };
                                    tracing::info!("📨 [Inbound] Received ping from {} (nonce: {})", peer.addr, nonce);

                                    match peer_registry.send_to_peer(&ip_str, pong_msg).await {
                                        Ok(()) => {
                                            tracing::info!("✅ [Inbound] Sent pong to {} (nonce: {})", peer.addr, nonce);
                                        }
                                        Err(e) => {
                                            tracing::warn!("❌ [Inbound] Failed to send pong to {}: {}", peer.addr, e);
                                        }
                                    }
                                }
                                NetworkMessage::Pong { nonce, timestamp: _ } => {
                                    // Inbound connections don't send pings, just log if we receive a pong
                                    tracing::debug!("📥 [Inbound] Received pong from {} (nonce: {})", peer.addr, nonce);
                                }
                                NetworkMessage::TransactionVoteRequest { txid } => {
                                    // Peer is requesting our vote on a transaction
                                    tracing::debug!("📥 Vote request from {} for TX {:?}", peer.addr, hex::encode(txid));

                                    // Get our preference (Accept/Reject) for this transaction
                                    let preference = if consensus.tx_pool.is_pending(txid) || consensus.tx_pool.get_pending(txid).is_some() {
                                        // We have this transaction pending/finalized
                                        "Accept".to_string()
                                    } else {
                                        // We don't have this transaction
                                        "Reject".to_string()
                                    };

                                    // Send our vote
                                    let vote_response = NetworkMessage::TransactionVoteResponse {
                                        txid: *txid,
                                        preference,
                                    };
                                    let _ = peer_registry.send_to_peer(&ip_str, vote_response).await;
                                }
                                NetworkMessage::TransactionVoteResponse { txid, preference } => {
                                    // Received a vote from a peer
                                    tracing::debug!("📥 Vote from {} for TX {:?}: {}", peer.addr, hex::encode(txid), preference);

                                    // Update our Avalanche consensus with this vote
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

                                    // Submit vote to Avalanche consensus
                                    // The consensus engine will update Snowball state
                                    consensus.avalanche.submit_vote(*txid, peer.addr.clone(), pref);

                                    tracing::debug!("✅ Vote recorded for TX {:?}", hex::encode(txid));
                                }
                                NetworkMessage::FinalityVoteBroadcast { vote } => {
                                    // Received a finality vote from a peer
                                    tracing::debug!("📥 Finality vote from {} for TX {:?}", peer.addr, hex::encode(vote.txid));

                                    // Accumulate the finality vote in consensus
                                    if let Err(e) = consensus.avalanche.accumulate_finality_vote(vote.clone()) {
                                        tracing::warn!("Failed to accumulate finality vote from {}: {}", peer.addr, e);
                                    } else {
                                        tracing::debug!("✅ Finality vote recorded from {}", peer.addr);
                                    }
                                }
                                NetworkMessage::TSCDBlockProposal { block } => {
                                    // Received a block proposal from the TSDC leader
                                    tracing::info!("📦 Received TSDC block proposal at height {} from {}", block.header.height, peer.addr);

                                    // Phase 3E.1: Cache the block
                                    let block_hash = block.hash();
                                    block_cache.insert(block_hash, block.clone());
                                    tracing::debug!("💾 Cached block {} for voting", hex::encode(block_hash));

                                    // Phase 3E.2: Look up validator weight from masternode registry
                                    let validator_id = "validator_node".to_string();
                                    let validator_weight = match masternode_registry.get(&validator_id).await {
                                        Some(info) => info.masternode.collateral,
                                        None => 1u64, // Default to 1 if not found
                                    };

                                    consensus.avalanche.generate_prepare_vote(block_hash, &validator_id, validator_weight);
                                    tracing::info!("✅ Generated prepare vote for block {} at height {}",
                                        hex::encode(block_hash), block.header.height);

                                    // Broadcast prepare vote to all peers
                                    let sig_bytes = vec![]; // TODO: Phase 3E.4: Sign with validator key
                                    let prepare_vote = NetworkMessage::TSCDPrepareVote {
                                        block_hash,
                                        voter_id: validator_id,
                                        signature: sig_bytes,
                                    };

                                    match broadcast_tx.send(prepare_vote) {
                                        Ok(receivers) => {
                                            tracing::info!("📤 Broadcast prepare vote to {} peers", receivers.saturating_sub(1));
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to broadcast prepare vote: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::TSCDPrepareVote { block_hash, voter_id, signature: _ } => {
                                    tracing::debug!("🗳️  Received prepare vote for block {} from {}",
                                        hex::encode(block_hash), voter_id);

                                    // Phase 3E.2: Look up voter weight from masternode registry
                                    let voter_weight = match masternode_registry.get(voter_id).await {
                                        Some(info) => info.masternode.collateral,
                                        None => 1u64, // Default to 1 if not found
                                    };

                                    // Phase 3E.4: Verify vote signature (stub - TODO: implement Ed25519 verification)
                                    // For now, we accept the vote; in production, verify the signature

                                    consensus.avalanche.accumulate_prepare_vote(*block_hash, voter_id.clone(), voter_weight);

                                    // Check if prepare consensus reached (>50% majority Avalanche)
                                    if consensus.avalanche.check_prepare_consensus(*block_hash) {
                                        tracing::info!("✅ Prepare consensus reached for block {}",
                                            hex::encode(block_hash));

                                        // Generate precommit vote with actual weight
                                        let validator_id = "validator_node".to_string();
                                        let validator_weight = match masternode_registry.get(&validator_id).await {
                                            Some(info) => info.masternode.collateral,
                                            None => 1u64,
                                        };

                                        consensus.avalanche.generate_precommit_vote(*block_hash, &validator_id, validator_weight);
                                        tracing::info!("✅ Generated precommit vote for block {}", hex::encode(block_hash));

                                        // Broadcast precommit vote
                                        let precommit_vote = NetworkMessage::TSCDPrecommitVote {
                                            block_hash: *block_hash,
                                            voter_id: validator_id,
                                            signature: vec![],
                                        };

                                        let _ = broadcast_tx.send(precommit_vote);
                                    }
                                }
                                NetworkMessage::TSCDPrecommitVote { block_hash, voter_id, signature: _ } => {
                                    tracing::debug!("🗳️  Received precommit vote for block {} from {}",
                                        hex::encode(block_hash), voter_id);

                                    // Phase 3E.2: Look up voter weight from masternode registry
                                    let voter_weight = match masternode_registry.get(voter_id).await {
                                        Some(info) => info.masternode.collateral,
                                        None => 1u64, // Default to 1 if not found
                                    };

                                    // Phase 3E.4: Verify vote signature (stub)
                                    // In production, verify Ed25519 signature here

                                    consensus.avalanche.accumulate_precommit_vote(*block_hash, voter_id.clone(), voter_weight);

                                    // Check if precommit consensus reached (>50% majority Avalanche)
                                    if consensus.avalanche.check_precommit_consensus(*block_hash) {
                                        tracing::info!("✅ Precommit consensus reached for block {}",
                                            hex::encode(block_hash));

                                        // Phase 3E.3: Finalization Callback
                                        // 1. Retrieve the block from cache
                                        if let Some((_, block)) = block_cache.remove(block_hash) {
                                            // 2. Collect precommit signatures (TODO: implement signature collection)
                                            let _signatures: Vec<Vec<u8>> = vec![]; // TODO: Collect actual signatures

                                            // 3. Phase 3E.3: Call tsdc.finalize_block_complete()
                                            // Note: This would be called through a TSDC module instance
                                            // For now, emit finalization event
                                            tracing::info!("🎉 Block {} finalized with consensus!", hex::encode(block_hash));
                                            tracing::info!("📦 Block height: {}, txs: {}", block.header.height, block.transactions.len());

                                            // 4. Emit finalization event
                                            // Calculate reward
                                            let height = block.header.height;
                                            let ln_height = if height == 0 { 0.0 } else { (height as f64).ln() };
                                            let block_subsidy = (100_000_000.0 * (1.0 + ln_height)) as u64;
                                            let tx_fees: u64 = block.transactions.iter().map(|tx| tx.fee_amount()).sum();
                                            let total_reward = block_subsidy + tx_fees;

                                            tracing::info!(
                                                "💰 Block {} rewards - subsidy: {}, fees: {}, total: {:.2} TIME",
                                                height,
                                                block_subsidy / 100_000_000,
                                                tx_fees / 100_000_000,
                                                total_reward as f64 / 100_000_000.0
                                            );
                                        } else {
                                            tracing::warn!("⚠️  Block {} not found in cache for finalization", hex::encode(block_hash));
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            failed_parse_count += 1;
                            // Try to parse to see what the error is
                            if let Err(parse_err) = serde_json::from_str::<NetworkMessage>(&line) {
                                tracing::warn!("❌ Failed to parse message {} from {}: {} | Raw: {} | Error: {}",
                                    failed_parse_count, peer.addr, line.trim(),
                                    line.chars().take(100).collect::<String>(), parse_err);
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
                                tracing::info!("📤 Sending block {} to peer {}", block.header.height, peer.addr);
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

    // Cleanup: mark inbound connection as disconnected
    connection_manager.mark_inbound_disconnected(&ip_str);
    tracing::info!("🔌 Peer {} disconnected (EOF)", peer.addr);

    Ok(())
}
