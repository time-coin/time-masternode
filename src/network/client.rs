use crate::blockchain::Blockchain;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::network::message::NetworkMessage;
use crate::peer_manager::PeerManager;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::time::{sleep, Duration};

pub struct NetworkClient {
    peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    connection_manager: Arc<ConnectionManager>,
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
            connection_manager: Arc::new(ConnectionManager::new()),
        }
    }

    /// Start persistent connections to all known peers
    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();
        let blockchain = self.blockchain.clone();
        let connection_manager = self.connection_manager.clone();

        tokio::spawn(async move {
            // Get initial list of known peers ONCE
            let peers = peer_manager.get_all_peers().await;

            // Connect to each peer (one connection task per peer, never duplicated)
            for peer_addr in peers.iter().take(6) {
                // Extract IP from address
                let ip = if let Some(colon_pos) = peer_addr.rfind(':') {
                    &peer_addr[..colon_pos]
                } else {
                    continue;
                };

                // Skip dead nodes that we know won't work
                if ip == "165.232.154.150" || ip == "178.128.199.144" {
                    continue;
                }

                // Skip if already connected/connecting
                if connection_manager.is_connected(ip).await {
                    continue;
                }

                // Mark as connecting
                if !connection_manager.mark_connecting(ip).await {
                    continue; // Already connecting
                }

                let cm = connection_manager.clone();
                let mr = masternode_registry.clone();
                let bc = blockchain.clone();
                let ip_str = ip.to_string();

                // Spawn ONE persistent connection task per peer
                tokio::spawn(async move {
                    let mut retry_delay = 5;
                    let mut consecutive_failures = 0;

                    loop {
                        match maintain_peer_connection(&ip_str, cm.clone(), mr.clone(), bc.clone())
                            .await
                        {
                            Ok(_) => {
                                tracing::info!("Connection to {} ended gracefully", ip_str);
                                consecutive_failures = 0;
                                retry_delay = 5; // Reset delay on graceful disconnect
                            }
                            Err(e) => {
                                consecutive_failures += 1;
                                tracing::warn!(
                                    "Connection to {} failed (attempt {}): {}",
                                    ip_str,
                                    consecutive_failures,
                                    e
                                );

                                // Stop trying after 10 consecutive failures
                                if consecutive_failures >= 10 {
                                    tracing::error!(
                                        "Giving up on {} after 10 failed attempts",
                                        ip_str
                                    );
                                    break;
                                }

                                // Exponential backoff up to 5 minutes
                                retry_delay = (retry_delay * 2).min(300);
                            }
                        }

                        // Mark as disconnected
                        cm.mark_disconnected(&ip_str).await;

                        // Wait before reconnecting
                        tracing::info!("Reconnecting to {} in {}s...", ip_str, retry_delay);
                        sleep(Duration::from_secs(retry_delay)).await;

                        // Mark as connecting again
                        cm.mark_connecting(&ip_str).await;
                    }

                    // Final cleanup
                    cm.mark_disconnected(&ip_str).await;
                });

                // Small delay between initial connection attempts
                sleep(Duration::from_millis(100)).await;
            }
        });
    }
}

/// Maintain a persistent connection to a peer
async fn maintain_peer_connection(
    ip: &str,
    connection_manager: Arc<ConnectionManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
) -> Result<(), String> {
    // Connect directly - connection manager just tracks we're connected
    let addr = format!("{}:24100", ip);
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    tracing::info!("✓ Connected to peer: {}", ip);

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

    tracing::debug!("📡 Sent handshake to {}", ip);

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

        tracing::info!("📡 Announced masternode to {}", ip);
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

    tracing::debug!("📡 Requested initial block height from {}", ip);

    // Request pending transactions (to catch any we missed during downtime)
    let tx_request = NetworkMessage::GetPendingTransactions;
    let msg_json =
        serde_json::to_string(&tx_request).map_err(|e| format!("Failed to serialize: {}", e))?;
    writer
        .write_all(format!("{}\n", msg_json).as_bytes())
        .await
        .map_err(|e| format!("Write failed: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("Flush failed: {}", e))?;

    tracing::debug!("📡 Requested pending transactions from {}", ip);

    // Request masternode list
    let mn_request = NetworkMessage::GetMasternodes;
    let msg_json =
        serde_json::to_string(&mn_request).map_err(|e| format!("Failed to serialize: {}", e))?;
    writer
        .write_all(format!("{}\n", msg_json).as_bytes())
        .await
        .map_err(|e| format!("Write failed: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("Flush failed: {}", e))?;

    tracing::debug!("📡 Requested masternode list from {}", ip);

    // Request peer list for peer discovery
    let peers_request = NetworkMessage::GetPeers;
    let msg_json =
        serde_json::to_string(&peers_request).map_err(|e| format!("Failed to serialize: {}", e))?;
    writer
        .write_all(format!("{}\n", msg_json).as_bytes())
        .await
        .map_err(|e| format!("Write failed: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("Flush failed: {}", e))?;

    tracing::debug!("📡 Requested peer list from {}", ip);

    // Read responses
    let mut line = String::new();
    tracing::info!("🔄 Starting message loop for peer {}", ip);

    loop {
        tokio::select! {
            // Send periodic heartbeat and sync check
            _ = heartbeat_interval.tick() => {
                tracing::debug!("💓 Sending heartbeat to {}", ip);

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
                            tracing::warn!("❌ Failed to write heartbeat to {}: {}", ip, e);
                            break;
                        }
                        if let Err(e) = writer.flush().await {
                            tracing::warn!("❌ Failed to flush heartbeat to {}: {}", ip, e);
                            break;
                        }
                    }
                }

                // Request peer height for sync check
                let sync_msg = NetworkMessage::GetBlockHeight;
                if let Ok(msg_json) = serde_json::to_string(&sync_msg) {
                    if let Err(e) = writer.write_all(format!("{}\n", msg_json).as_bytes()).await {
                        tracing::warn!("❌ Failed to write sync request to {}: {}", ip, e);
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        tracing::warn!("❌ Failed to flush sync request to {}: {}", ip, e);
                        break;
                    }
                }

                // Request UTXO state hash for verification (every 10 minutes)
                let utxo_check_msg = NetworkMessage::GetUTXOStateHash;
                if let Ok(msg_json) = serde_json::to_string(&utxo_check_msg) {
                    if let Err(e) = writer.write_all(format!("{}\n", msg_json).as_bytes()).await {
                        tracing::warn!("❌ Failed to write UTXO check to {}: {}", ip, e);
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        tracing::warn!("❌ Failed to flush UTXO check to {}: {}", ip, e);
                        break;
                    }
                }
            }

            // Read incoming messages
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => {
                        tracing::info!("🔌 Connection to {} closed by peer (EOF)", ip);
                        break;
                    }
                    Ok(n) => {
                        tracing::debug!("📨 Received {} bytes from {}: {}", n, ip, line.trim());
                        if let Ok(msg) = serde_json::from_str::<NetworkMessage>(&line) {
                            match msg {
                                NetworkMessage::MasternodeAnnouncement { address: mn_addr, reward_address, tier, public_key } => {
                                    // Extract just IP from the announced address
                                    let ip = mn_addr.split(':').next().unwrap_or(&mn_addr).to_string();
                                    if let Err(e) = masternode_registry.register_masternode(ip.clone(), reward_address, tier, public_key).await {
                                        tracing::warn!("Failed to register masternode {}: {}", ip, e);
                                    }
                                }
                                NetworkMessage::BlockHeightResponse(remote_height) => {
                                    let local_height = blockchain.get_height().await;
                                    if remote_height > local_height {
                                        tracing::info!("📥 Peer has height {}, we have {}. Requesting blocks...", remote_height, local_height);
                                        // If we have no blocks, start from genesis (block 0)
                                        let start_height = if local_height == 0 { 0 } else { local_height + 1 };
                                        let req = NetworkMessage::GetBlocks(start_height, remote_height);
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
                                NetworkMessage::PendingTransactionsResponse(transactions) => {
                                    if !transactions.is_empty() {
                                        tracing::info!("📩 Received {} pending transaction(s) from peer", transactions.len());
                                        for tx in transactions {
                                            if let Err(e) = blockchain.add_pending_transaction(tx).await {
                                                tracing::debug!("Transaction already known or invalid: {}", e);
                                            }
                                        }
                                    }
                                }
                                NetworkMessage::MasternodesResponse(masternodes) => {
                                    if !masternodes.is_empty() {
                                        tracing::info!("📩 Received {} masternode(s) from peer", masternodes.len());

                                        // Get local masternode address to skip self-registration
                                        let local_mn = masternode_registry.get_local_masternode().await;
                                        let local_address = local_mn.as_ref().map(|mn| {
                                            // Strip port if present for comparison
                                            let addr = mn.masternode.address.clone();
                                            addr.split(':').next().unwrap_or(&addr).to_string()
                                        });

                                        if let Some(ref local_mn_info) = local_mn {
                                            tracing::info!("Local masternode: {} (reward: {})",
                                                local_mn_info.masternode.address,
                                                local_mn_info.reward_address);
                                        }
                                        if let Some(ref addr) = local_address {
                                            tracing::debug!("Local masternode IP (for comparison): {}", addr);
                                        }

                                        let mut registered = 0;
                                        for mn_data in masternodes {
                                            tracing::debug!("Processing masternode: {} (reward: {})", mn_data.address, mn_data.reward_address);

                                            // Skip if this is our own masternode (compare IPs without port)
                                            if let Some(ref local_addr) = local_address {
                                                let peer_addr = mn_data.address.split(':').next().unwrap_or(&mn_data.address);
                                                if peer_addr == local_addr {
                                                    tracing::info!("⏭️  Skipping self-registration for {}", mn_data.address);
                                                    continue;
                                                }
                                            }

                                            let mn = crate::types::Masternode {
                                                address: mn_data.address.clone(),
                                                wallet_address: mn_data.reward_address.clone(),
                                                collateral: mn_data.tier.collateral(),
                                                tier: mn_data.tier.clone(),
                                                public_key: mn_data.public_key,
                                                registered_at: std::time::SystemTime::now()
                                                    .duration_since(std::time::UNIX_EPOCH)
                                                    .unwrap()
                                                    .as_secs(),
                                            };

                                            if let Err(e) = masternode_registry.register(mn, mn_data.reward_address.clone()).await {
                                                tracing::debug!("Masternode already registered or invalid: {}", e);
                                            } else {
                                                registered += 1;
                                            }
                                        }

                                        if registered > 0 {
                                            tracing::info!("✅ Registered {} new masternode(s)", registered);
                                        }
                                    }
                                }
                                NetworkMessage::UTXOStateHashResponse { hash, height, utxo_count } => {
                                    let local_height = blockchain.get_height().await;
                                    let local_hash = blockchain.get_utxo_state_hash().await;
                                    let local_count = blockchain.get_utxo_count().await;

                                    if height == local_height && hash != local_hash {
                                        tracing::warn!(
                                            "⚠️ UTXO state mismatch with peer at height {}! Local: {} UTXOs (hash: {}), Peer: {} UTXOs (hash: {})",
                                            height,
                                            local_count,
                                            hex::encode(&local_hash[..8]),
                                            utxo_count,
                                            hex::encode(&hash[..8])
                                        );

                                        // Request full UTXO set from peer to reconcile
                                        let request = NetworkMessage::GetUTXOSet;
                                        if let Ok(json) = serde_json::to_string(&request) {
                                            let _ = writer.write_all(format!("{}\n", json).as_bytes()).await;
                                            let _ = writer.flush().await;
                                            tracing::info!("📥 Requesting full UTXO set from peer for reconciliation");
                                        }
                                    } else if height == local_height {
                                        tracing::debug!("✅ UTXO state matches peer at height {}", height);
                                    }
                                }
                                NetworkMessage::UTXOSetResponse(utxos) => {
                                    tracing::info!("📥 Received {} UTXOs from peer for reconciliation", utxos.len());
                                    blockchain.reconcile_utxo_state(utxos).await;
                                }
                                _ => {}
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        tracing::info!("🔌 Connection to {} ended: {}", ip, e);
                        break;
                    }
                }
            }
        }
    }

    // Mark as disconnected when done
    connection_manager.mark_disconnected(ip).await;

    Ok(())
}
