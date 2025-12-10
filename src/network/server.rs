use crate::consensus::ConsensusEngine;
use crate::network::blacklist::IPBlacklist;
use crate::network::message::{NetworkMessage, Subscription, UTXOStateChange};
use crate::network::rate_limiter::RateLimiter;
use crate::types::OutPoint;
use crate::utxo_manager::UTXOStateManager;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
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
}

pub struct PeerConnection {
    pub addr: String,
    #[allow(dead_code)]
    pub is_masternode: bool,
}

impl NetworkServer {
    pub async fn new(
        bind_addr: &str,
        utxo_manager: Arc<UTXOStateManager>,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
        blockchain: Arc<crate::blockchain::Blockchain>,
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

        loop {
            let (stream, addr) = self.listener.accept().await?;
            let addr_str = addr.to_string();

            // Extract IP address
            let ip: IpAddr = addr.ip();

            // Check blacklist BEFORE accepting connection
            {
                let mut blacklist = self.blacklist.write().await;
                if let Some(reason) = blacklist.is_blacklisted(ip) {
                    tracing::warn!("🚫 Rejected blacklisted IP {}: {}", ip, reason);
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
) -> Result<(), std::io::Error> {
    // Extract IP from address
    let ip: IpAddr = peer
        .addr
        .split(':')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1".parse().unwrap());

    tracing::info!("🔌 New peer connection from: {}", peer.addr);
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);
    let mut line = String::new();
    let mut failed_parse_count = 0;
    let mut handshake_done = false;

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

                                        // Request peer list for peer discovery
                                        let get_peers_msg = NetworkMessage::GetPeers;
                                        if let Ok(json) = serde_json::to_string(&get_peers_msg) {
                                            let _ = writer.write_all(json.as_bytes()).await;
                                            let _ = writer.write_all(b"\n").await;
                                            let _ = writer.flush().await;
                                        }

                                        line.clear();
                                        continue;
                                    }
                                    _ => {
                                        tracing::warn!("🚫 Rejecting {} - first message must be handshake", peer.addr);
                                        blacklist.write().await.record_violation(
                                            ip,
                                            "No handshake sent"
                                        );
                                        break;
                                    }
                                }
                            }

                            tracing::debug!("📦 Parsed message type from {}: {:?}", peer.addr, std::mem::discriminant(&msg));
                            let ip_str = &peer.addr;
                            let mut limiter = rate_limiter.write().await;

                            match &msg {
                                NetworkMessage::TransactionBroadcast(tx) => {
                                    if limiter.check("tx", ip_str) {
                                        if let Err(e) = consensus.process_transaction(tx.clone()).await {
                                            eprintln!("Tx rejected: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::UTXOStateQuery(outpoints) => {
                                    if limiter.check("utxo_query", ip_str) {
                                        let mut responses = Vec::new();
                                        for op in outpoints {
                                            if let Some(state) = utxo_mgr.get_state(op).await {
                                                responses.push((op.clone(), state));
                                            }
                                        }
                                        let reply = NetworkMessage::UTXOStateResponse(responses);
                                        if let Ok(json) = serde_json::to_string(&reply) {
                                            let _ = writer.write_all(json.as_bytes()).await;
                                            let _ = writer.write_all(b"\n").await;
                                            let _ = writer.flush().await;
                                        }
                                    }
                                }
                                NetworkMessage::Subscribe(sub) => {
                                    if limiter.check("subscribe", ip_str) {
                                        subs.write().await.insert(sub.id.clone(), sub.clone());
                                    }
                                }
                                NetworkMessage::GetBlockHeight => {
                                    let height = blockchain.get_height().await;
                                    let reply = NetworkMessage::BlockHeightResponse(height);
                                    if let Ok(json) = serde_json::to_string(&reply) {
                                        let _ = writer.write_all(json.as_bytes()).await;
                                        let _ = writer.write_all(b"\n").await;
                                        let _ = writer.flush().await;
                                    }
                                }
                                NetworkMessage::GetPendingTransactions => {
                                    // Get pending transactions from mempool
                                    let pending_txs = blockchain.get_pending_transactions().await;
                                    let reply = NetworkMessage::PendingTransactionsResponse(pending_txs);
                                    if let Ok(json) = serde_json::to_string(&reply) {
                                        let _ = writer.write_all(json.as_bytes()).await;
                                        let _ = writer.write_all(b"\n").await;
                                        let _ = writer.flush().await;
                                    }
                                }
                                NetworkMessage::GetBlocks(start, end) => {
                                    let mut blocks = Vec::new();
                                    for h in *start..=(*end).min(start + 100) {
                                        if let Ok(block) = blockchain.get_block_by_height(h).await {
                                            blocks.push(block);
                                        }
                                    }
                                    let reply = NetworkMessage::BlocksResponse(blocks);
                                    if let Ok(json) = serde_json::to_string(&reply) {
                                        let _ = writer.write_all(json.as_bytes()).await;
                                        let _ = writer.write_all(b"\n").await;
                                        let _ = writer.flush().await;
                                    }
                                }
                                NetworkMessage::MasternodeAnnouncement { address: _, reward_address, tier, public_key } => {
                                    // Extract just the IP (no port) from the peer connection
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("").to_string();

                                    if peer_ip.is_empty() {
                                        tracing::warn!("❌ Invalid peer IP from {}", peer.addr);
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
                                        },
                                        Err(e) => {
                                            tracing::warn!("❌ Failed to register masternode {}: {}", peer_ip, e);
                                        }
                                    }
                                }
                                NetworkMessage::GetPeers => {
                                    // Share our known peers (excluding the requester)
                                    let all_peers = peer_manager.get_all_peers().await;
                                    let peer_ip = peer.addr.split(':').next().unwrap_or("");
                                    let peer_list: Vec<String> = all_peers
                                        .into_iter()
                                        .filter(|p| !p.starts_with(peer_ip)) // Don't send them their own IP
                                        .map(|ip| format!("{}:24100", ip)) // Add port
                                        .collect();

                                    tracing::info!("📤 Sending {} peer(s) to {}", peer_list.len(), peer.addr);
                                    let reply = NetworkMessage::PeersResponse(peer_list);
                                    if let Err(e) = write_message(&mut writer, &reply).await {
                                        tracing::warn!("❌ Failed to send peers response: {}", e);
                                    }
                                }
                                NetworkMessage::PeersResponse(peers) => {
                                    tracing::info!("📥 Received {} peer(s) from {}", peers.len(), peer.addr);
                                    let mut new_peers = 0;

                                    // Add new peers to our peer manager and attempt connections
                                    for peer_addr in peers {
                                        // Add to peer manager
                                        if let Err(e) = peer_manager.add_peer(&peer_addr).await {
                                            tracing::debug!("Failed to add peer {}: {}", peer_addr, e);
                                            continue;
                                        }

                                        new_peers += 1;

                                        // Extract IP and attempt connection
                                        let peer_ip = peer_addr.split(':').next().unwrap_or(&peer_addr).to_string();

                                        // Spawn connection attempt in background
                                        let conn_manager_clone = connection_manager.clone();
                                        let peer_ip_clone = peer_ip.clone();
                                        tokio::spawn(async move {
                                            let _ = conn_manager_clone.connect(&peer_ip_clone).await;
                                        });
                                    }

                                    if new_peers > 0 {
                                        tracing::info!("🔍 Discovered {} new peer(s), connecting...", new_peers);
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            failed_parse_count += 1;
                            tracing::warn!("❌ Failed to parse message {} from {}: {}", failed_parse_count, peer.addr, line.trim());
                            // Record violation and check if should ban
                            let should_ban = blacklist.write().await.record_violation(
                                ip,
                                "Failed to parse message"
                            );
                            if should_ban || failed_parse_count >= 3 {
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
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = writer.write_all(json.as_bytes()).await;
                            let _ = writer.write_all(b"\n").await;
                            let _ = writer.flush().await;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    Ok(())
}
