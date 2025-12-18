use crate::consensus::ConsensusEngine;
use crate::network::blacklist::IPBlacklist;
use crate::network::message::{NetworkMessage, Subscription, UTXOStateChange};
use crate::network::peer_state::PeerStateManager;
use crate::network::rate_limiter::RateLimiter;
use crate::types::OutPoint;
use crate::utxo_manager::UTXOStateManager;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::sync::RwLock;

pub struct NetworkServer {
    pub listener: TcpListener,
    pub peers: Arc<RwLock<HashMap<String, PeerConnection>>>,
    pub subscriptions: Arc<RwLock<HashMap<String, Subscription>>>,
    pub tx_notifier: broadcast::Sender<NetworkMessage>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub consensus: Arc<ConsensusEngine>,
    pub rate_limiter: Arc<RwLock<RateLimiter>>,
    pub blacklist: Arc<RwLock<IPBlacklist>>,
    pub masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    pub blockchain: Arc<crate::blockchain::Blockchain>,
    pub peer_manager: Arc<crate::peer_manager::PeerManager>,
    pub seen_blocks: Arc<RwLock<HashSet<u64>>>, // Track seen block heights
    pub seen_transactions: Arc<RwLock<HashSet<[u8; 32]>>>, // Track seen transaction hashes
    pub connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    pub peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    #[allow(dead_code)]
    pub peer_state: Arc<PeerStateManager>,
    pub local_ip: Option<String>, // Our own public IP (without port) to avoid self-connection
}

pub struct PeerConnection {
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
            seen_blocks: Arc::new(RwLock::new(HashSet::new())),
            seen_transactions: Arc::new(RwLock::new(HashSet::new())),
            connection_manager,
            peer_registry,
            peer_state,
            local_ip,
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

        // Spawn cleanup task for seen transactions cache
        let seen_txs_cleanup = self.seen_transactions.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(600)).await; // Every 10 minutes
                let mut seen = seen_txs_cleanup.write().await;
                let old_size = seen.len();

                // Keep only recent 10,000 transactions to prevent unbounded memory growth
                if old_size > 10000 {
                    seen.clear();
                    tracing::debug!("🧹 Cleared seen_transactions cache ({} entries)", old_size);
                }
            }
        });

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

            let peer = PeerConnection {
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
    peer: PeerConnection,
    _peers: Arc<RwLock<HashMap<String, PeerConnection>>>,
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
    seen_blocks: Arc<RwLock<HashSet<u64>>>,
    seen_transactions: Arc<RwLock<HashSet<[u8; 32]>>>,
    connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    _local_ip: Option<String>,
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
                                        let has_outbound = connection_manager.is_connected(&ip_str).await;

                                        if has_outbound {
                                            // We have an outbound connection to this peer
                                            // Use deterministic tie-breaking based on IP comparison
                                            let should_we_connect = connection_manager.should_connect_to(&ip_str).await;

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
                                            connection_manager.remove(&ip_str).await;
                                        }

                                        // Mark this inbound connection
                                        connection_manager.mark_inbound(&ip_str).await;

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

                                        // Request peer list for peer discovery
                                        let get_peers_msg = NetworkMessage::GetPeers;
                                        let _ = peer_registry.send_to_peer(&ip_str, get_peers_msg).await;

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
                                        // Check if we've already seen this transaction
                                        let txid = tx.txid();
                                        let already_seen = {
                                            let mut seen = seen_transactions.write().await;
                                            !seen.insert(txid)
                                        };

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
                                NetworkMessage::TransactionVote(vote) => {
                                    if limiter.check("vote", &ip_str) {
                                        let txid = vote.txid;
                                        tracing::info!("🗳️  Received vote for {} from {} (approve: {})",
                                            hex::encode(txid), vote.voter, vote.approve);

                                        match consensus.handle_transaction_vote(vote.clone()).await {
                                            Ok(_) => {
                                                tracing::debug!("✅ Vote processed for {}", hex::encode(txid));

                                                // Gossip vote to other peers
                                                match broadcast_tx.send(msg.clone()) {
                                                    Ok(receivers) => {
                                                        tracing::debug!("🔄 Gossiped vote to {} peer(s)", receivers.saturating_sub(1));
                                                    }
                                                    Err(e) => {
                                                        tracing::debug!("Failed to gossip vote: {}", e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("❌ Vote rejected: {}", e);
                                            }
                                        }
                                    }
                                }
                                NetworkMessage::TransactionFinalized { txid, votes } => {
                                    tracing::info!("✅ Transaction {} finalized with {} votes (from {})",
                                        hex::encode(*txid), votes, peer.addr);

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
                                NetworkMessage::TransactionRejected { txid, reason } => {
                                    tracing::warn!("❌ Transaction {} rejected: {} (from {})",
                                        hex::encode(*txid), reason, peer.addr);

                                    // Gossip rejection to other peers
                                    match broadcast_tx.send(msg.clone()) {
                                        Ok(receivers) => {
                                            tracing::debug!("🔄 Gossiped rejection to {} peer(s)", receivers.saturating_sub(1));
                                        }
                                        Err(e) => {
                                            tracing::debug!("Failed to gossip rejection: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::UTXOStateQuery(outpoints) => {
                                    if limiter.check("utxo_query", &ip_str) {
                                        let mut responses = Vec::new();
                                        for op in outpoints {
                                            if let Some(state) = utxo_mgr.get_state(op).await {
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
                                    let pending_txs = blockchain.get_pending_transactions().await;
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
                                    tracing::debug!("📥 Received GetMasternodes request from {}", peer.addr);
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
                                    tracing::debug!("📤 Sent {} masternode(s) to {}", all_masternodes.len(), peer.addr);
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
                                NetworkMessage::BlockAnnouncement(block) => {
                                    let block_height = block.header.height;

                                    // Check if we've already seen this block
                                    let already_seen = {
                                        let mut seen = seen_blocks.write().await;
                                        if seen.contains(&block_height) {
                                            true
                                        } else {
                                            seen.insert(block_height);

                                            // Keep cache from growing forever - remove old blocks
                                            if seen.len() > 1000 {
                                                let min_height = block_height.saturating_sub(1000);
                                                seen.retain(|&h| h > min_height);
                                            }
                                            false
                                        }
                                    };

                                    if already_seen {
                                        tracing::debug!("🔁 Ignoring duplicate block {} from {}", block_height, peer.addr);
                                        line.clear();
                                        continue;
                                    }

                                    tracing::info!("📥 Received block {} announcement from {}", block_height, peer.addr);

                                    // Add block to our blockchain
                                    match blockchain.add_block(block.clone()).await {
                                        Ok(()) => {
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
                                // Heartbeat Messages
                                NetworkMessage::HeartbeatBroadcast(heartbeat) => {
                                    tracing::debug!("💓 Received heartbeat from {} seq {}",
                                        heartbeat.masternode_address, heartbeat.sequence_number);

                                    // Process heartbeat through masternode registry
                                    if let Err(e) = masternode_registry.receive_heartbeat_broadcast(heartbeat.clone()).await {
                                        tracing::warn!("Failed to process heartbeat: {}", e);
                                    }
                                }
                                NetworkMessage::HeartbeatAttestation(attestation) => {
                                    tracing::debug!("✍️ Received heartbeat attestation from {}",
                                        attestation.witness_address);

                                    // Process attestation through masternode registry
                                    if let Err(e) = masternode_registry.receive_attestation_broadcast(attestation.clone()).await {
                                        tracing::warn!("Failed to process attestation: {}", e);
                                    }
                                }
                                // BFT Consensus Messages
                                NetworkMessage::BlockProposal { .. } |
                                NetworkMessage::BlockVote { .. } |
                                NetworkMessage::BlockCommit { .. } => {
                                    tracing::debug!("📥 Received BFT message from {}", peer.addr);

                                    // Handle BFT message through blockchain
                                    if let Err(e) = blockchain.handle_bft_message(msg.clone()).await {
                                        tracing::warn!("Failed to handle BFT message: {}", e);
                                    }

                                    // Gossip BFT messages to other peers
                                    match broadcast_tx.send(msg.clone()) {
                                        Ok(receivers) => {
                                            tracing::debug!("🔄 Gossiped BFT message to {} peer(s)", receivers.saturating_sub(1));
                                        }
                                        Err(e) => {
                                            tracing::debug!("Failed to gossip BFT message: {}", e);
                                        }
                                    }
                                }
                                // Health Check Messages
                                NetworkMessage::Ping { nonce, timestamp: _ } => {
                                    // Respond to ping with pong
                                    let pong_msg = NetworkMessage::Pong {
                                        nonce: *nonce,
                                        timestamp: chrono::Utc::now().timestamp(),
                                    };
                                    tracing::info!("📨 [INBOUND] Received ping from {} (nonce: {}), sending pong", peer.addr, nonce);
                                    tracing::debug!("🔍 Sending pong to IP: {} (peer.addr: {})", ip_str, peer.addr);

                                    match peer_registry.send_to_peer(&ip_str, pong_msg).await {
                                        Ok(()) => {
                                            tracing::info!("✅ [INBOUND] Sent pong to {} (nonce: {})", peer.addr, nonce);
                                        }
                                        Err(e) => {
                                            tracing::error!("❌ [INBOUND] Failed to send pong to {} (IP: {}): {}", peer.addr, ip_str, e);
                                        }
                                    }
                                }
                                NetworkMessage::Pong { nonce, timestamp: _ } => {
                                    // Inbound connections don't send pings, just log if we receive a pong
                                    tracing::info!("📥 [INBOUND] Received unexpected pong from {} (nonce: {})", peer.addr, nonce);
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
    connection_manager.mark_inbound_disconnected(&ip_str).await;
    tracing::info!("🔌 Peer {} disconnected (EOF)", peer.addr);

    Ok(())
}
