use crate::blockchain::Blockchain;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::message::NetworkMessage;
use crate::peer_manager::PeerManager;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};

pub struct NetworkClient {
    peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
}

impl NetworkClient {
    pub fn new(
        peer_manager: Arc<PeerManager>,
        masternode_registry: Arc<MasternodeRegistry>,
        blockchain: Arc<Blockchain>,
    ) -> Self {
        Self {
            peer_manager,
            masternode_registry,
            blockchain,
        }
    }

    /// Start persistent connections to all known peers
    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();
        let blockchain = self.blockchain.clone();

        // Track active connections to prevent duplicates
        let active_connections: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        tokio::spawn(async move {
            loop {
                // Get list of known peers
                let peers = peer_manager.get_all_peers().await;

                // Connect to each peer if not already connected
                for peer_addr in peers.iter().take(6) {
                    let mut connections = active_connections.lock().await;

                    // Skip if already connected
                    if connections.contains(peer_addr) {
                        continue;
                    }

                    // Mark as connected
                    connections.insert(peer_addr.clone());
                    drop(connections);

                    let pm = peer_manager.clone();
                    let mr = masternode_registry.clone();
                    let bc = blockchain.clone();
                    let addr = peer_addr.clone();
                    let active_conns = active_connections.clone();

                    tokio::spawn(async move {
                        // Single connection attempt - no reconnection loop
                        match maintain_peer_connection(&addr, pm.clone(), mr.clone(), bc.clone())
                            .await
                        {
                            Ok(_) => tracing::info!("Connection to {} ended gracefully", addr),
                            Err(e) => tracing::debug!("Connection to {} failed: {}", addr, e),
                        }

                        // Remove from active connections when done
                        active_conns.lock().await.remove(&addr);
                    });
                }

                // Check for new peers every 60 seconds
                sleep(Duration::from_secs(60)).await;
            }
        });
    }
}

/// Maintain a persistent connection to a peer
async fn maintain_peer_connection(
    address: &str,
    _peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
) -> Result<(), String> {
    // Try to connect
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    tracing::info!("✓ Connected to peer: {}", address);

    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    // Send handshake FIRST
    let handshake = NetworkMessage::Handshake {
        magic: *b"TIME",
        protocol_version: 1,
        network: "Testnet".to_string(),
    };
    let handshake_json = serde_json::to_string(&handshake)
        .map_err(|e| format!("Failed to serialize handshake: {}", e))?;
    writer
        .write_all(format!("{}\n", handshake_json).as_bytes())
        .await
        .map_err(|e| format!("Failed to send handshake: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("Failed to flush handshake: {}", e))?;

    tracing::debug!("📡 Sent handshake to {}", address);

    // Announce our masternode if we are one
    if let Some(local_mn) = masternode_registry.get_local_masternode().await {
        let announce_msg = NetworkMessage::MasternodeAnnouncement {
            address: local_mn.masternode.address.clone(),
            reward_address: local_mn.reward_address.clone(),
            tier: local_mn.masternode.tier,
            public_key: local_mn.masternode.public_key,
        };

        let msg_json = serde_json::to_string(&announce_msg)
            .map_err(|e| format!("Failed to serialize: {}", e))?;

        writer
            .write_all(format!("{}\n", msg_json).as_bytes())
            .await
            .map_err(|e| format!("Write failed: {}", e))?;
        writer
            .flush()
            .await
            .map_err(|e| format!("Flush failed: {}", e))?;

        tracing::info!("📡 Announced masternode to {}", address);
    }

    // Send heartbeat and sync check every 2 minutes (blocks are every 10 minutes)
    let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(120));

    // Initial height request
    let sync_msg = NetworkMessage::GetBlockHeight;
    let msg_json =
        serde_json::to_string(&sync_msg).map_err(|e| format!("Failed to serialize: {}", e))?;
    writer
        .write_all(format!("{}\n", msg_json).as_bytes())
        .await
        .map_err(|e| format!("Write failed: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("Flush failed: {}", e))?;

    tracing::debug!("📡 Requested initial block height from {}", address);

    // Read responses
    let mut line = String::new();
    tracing::info!("🔄 Starting message loop for peer {}", address);

    loop {
        tokio::select! {
            // Send periodic heartbeat and sync check
            _ = heartbeat_interval.tick() => {
                tracing::debug!("💓 Sending heartbeat to {}", address);

                // Send masternode announcement
                if let Some(local_mn) = masternode_registry.get_local_masternode().await {
                    let heartbeat_msg = NetworkMessage::MasternodeAnnouncement {
                        address: local_mn.masternode.address.clone(),
                        reward_address: local_mn.reward_address.clone(),
                        tier: local_mn.masternode.tier,
                        public_key: local_mn.masternode.public_key,
                    };
                    if let Ok(msg_json) = serde_json::to_string(&heartbeat_msg) {
                        if let Err(e) = writer.write_all(format!("{}\n", msg_json).as_bytes()).await {
                            tracing::warn!("❌ Failed to write heartbeat to {}: {}", address, e);
                            break;
                        }
                        if let Err(e) = writer.flush().await {
                            tracing::warn!("❌ Failed to flush heartbeat to {}: {}", address, e);
                            break;
                        }
                    }
                }

                // Request peer height for sync check
                let sync_msg = NetworkMessage::GetBlockHeight;
                if let Ok(msg_json) = serde_json::to_string(&sync_msg) {
                    if let Err(e) = writer.write_all(format!("{}\n", msg_json).as_bytes()).await {
                        tracing::warn!("❌ Failed to write sync request to {}: {}", address, e);
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        tracing::warn!("❌ Failed to flush sync request to {}: {}", address, e);
                        break;
                    }
                }
            }

            // Read incoming messages
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        tracing::info!("🔌 Connection to {} closed by peer (EOF)", address);
                        break;
                    }
                    Ok(n) => {
                        tracing::debug!("📨 Received {} bytes from {}: {}", n, address, line.trim());
                        if let Ok(msg) = serde_json::from_str::<NetworkMessage>(&line) {
                            match msg {
                                NetworkMessage::MasternodeAnnouncement { address: mn_addr, reward_address, tier, public_key } => {
                                    if let Err(e) = masternode_registry.register_masternode(mn_addr.clone(), reward_address, tier, public_key).await {
                                        tracing::warn!("Failed to register masternode {}: {}", mn_addr, e);
                                    }
                                }
                                NetworkMessage::BlockHeightResponse(remote_height) => {
                                    let local_height = blockchain.get_height().await;
                                    if remote_height > local_height {
                                        tracing::info!("📥 Peer has height {}, we have {}. Requesting blocks...", remote_height, local_height);
                                        let req = NetworkMessage::GetBlocks(local_height + 1, remote_height);
                                        if let Ok(json) = serde_json::to_string(&req) {
                                            let _ = writer.write_all(format!("{}\n", json).as_bytes()).await;
                                            let _ = writer.flush().await;
                                        }
                                    }
                                }
                                NetworkMessage::BlocksResponse(blocks) => {
                                    tracing::info!("📦 Received {} blocks from peer", blocks.len());
                                    for block in blocks {
                                        if let Err(e) = blockchain.add_block(block).await {
                                            tracing::warn!("Failed to add block: {}", e);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        tracing::warn!("❌ Read error from {}: {}", address, e);
                        break;
                    }
                }
            }
        }
    }

    tracing::info!("🔌 Connection to {} ended", address);
    Ok(())
}
