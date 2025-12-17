use crate::blockchain::Blockchain;
use crate::heartbeat_attestation::HeartbeatAttestationSystem;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::NetworkType;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::time::{sleep, Duration};

pub struct NetworkClient {
    peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    attestation_system: Arc<HeartbeatAttestationSystem>,
    connection_manager: Arc<ConnectionManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    p2p_port: u16,
    max_peers: usize,
    reserved_masternode_slots: usize, // Reserved slots for masternodes
    local_ip: Option<String>,         // Our own public IP (without port) to avoid self-connection
}

impl NetworkClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        peer_manager: Arc<PeerManager>,
        masternode_registry: Arc<MasternodeRegistry>,
        blockchain: Arc<Blockchain>,
        attestation_system: Arc<HeartbeatAttestationSystem>,
        network_type: NetworkType,
        max_peers: usize,
        connection_manager: Arc<ConnectionManager>,
        peer_registry: Arc<PeerConnectionRegistry>,
        local_ip: Option<String>, // Our own public IP to avoid self-connection
    ) -> Self {
        // Reserve 40% of slots for masternodes, minimum 20 slots, max 30 slots
        let reserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30);

        Self {
            peer_manager,
            masternode_registry,
            blockchain,
            attestation_system,
            connection_manager,
            peer_registry,
            p2p_port: network_type.default_p2p_port(),
            max_peers,
            reserved_masternode_slots,
            local_ip,
        }
    }

    /// Start persistent connections to all known peers
    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();
        let blockchain = self.blockchain.clone();
        let attestation_system = self.attestation_system.clone();
        let connection_manager = self.connection_manager.clone();
        let peer_registry = self.peer_registry.clone();
        let p2p_port = self.p2p_port;
        let max_peers = self.max_peers;
        let reserved_masternode_slots = self.reserved_masternode_slots;
        let local_ip = self.local_ip.clone();

        tokio::spawn(async move {
            tracing::info!(
                "🔌 Starting network client (max peers: {}, reserved for masternodes: {})",
                max_peers,
                reserved_masternode_slots
            );

            if let Some(ref ip) = local_ip {
                tracing::info!("🏠 Local IP: {} (will skip self-connections)", ip);
            }

            // PHASE 1: Connect to all active masternodes FIRST (priority)
            let masternodes = masternode_registry.list_active().await;
            tracing::info!(
                "🎯 Connecting to {} active masternode(s) with priority...",
                masternodes.len()
            );

            let mut masternode_connections = 0;
            for mn in masternodes.iter().take(reserved_masternode_slots) {
                let ip = mn.masternode.address.clone();

                // CRITICAL FIX: Skip if this is our own IP
                if let Some(ref local) = local_ip {
                    if ip == *local {
                        tracing::info!("⏭️  [PHASE1-MN] Skipping self-connection to {}", ip);
                        continue;
                    }

                    // CRITICAL FIX: Only connect if our IP < peer IP (deterministic direction)
                    if local.as_str() >= ip.as_str() {
                        tracing::debug!("⏸️  [PHASE1-MN] Skipping outbound to {} (they should connect to us: {} >= {})", ip, local, ip);
                        continue;
                    }
                }

                tracing::info!("🔗 [PHASE1-MN] Initiating priority connection to: {}", ip);

                if connection_manager.is_connected(&ip).await {
                    tracing::debug!("Already connected to masternode {}", ip);
                    masternode_connections += 1;
                    continue;
                }

                if !connection_manager.mark_connecting(&ip).await {
                    tracing::debug!("[PHASE1-MN] Already connecting to {}, skipping", ip);
                    continue;
                }

                masternode_connections += 1;
                spawn_connection_task(
                    ip,
                    p2p_port,
                    connection_manager.clone(),
                    masternode_registry.clone(),
                    blockchain.clone(),
                    attestation_system.clone(),
                    peer_manager.clone(),
                    peer_registry.clone(),
                    true, // is_masternode flag
                    local_ip.clone(),
                );

                sleep(Duration::from_millis(100)).await;
            }

            tracing::info!(
                "✅ Connected to {} masternode(s), {} slots available for regular peers",
                masternode_connections,
                max_peers.saturating_sub(masternode_connections)
            );

            // PHASE 2: Fill remaining slots with regular peers
            let available_slots = max_peers.saturating_sub(masternode_connections);
            if available_slots > 0 {
                let peers = peer_manager.get_all_peers().await;

                // Deduplicate peers by IP (remove port) to prevent duplicate connections
                let mut seen_ips = std::collections::HashSet::new();
                let unique_peers: Vec<String> = peers
                    .into_iter()
                    .filter_map(|peer_addr| {
                        let ip = if let Some(colon_pos) = peer_addr.rfind(':') {
                            &peer_addr[..colon_pos]
                        } else {
                            &peer_addr
                        };

                        // Only keep first occurrence of each IP
                        if seen_ips.insert(ip.to_string()) {
                            Some(ip.to_string())
                        } else {
                            None
                        }
                    })
                    .collect();

                tracing::info!(
                    "🔌 Filling {} remaining slot(s) with {} unique regular peers",
                    available_slots,
                    unique_peers.len()
                );

                for ip in unique_peers.iter().take(available_slots) {
                    // CRITICAL FIX: Skip if this is our own IP
                    if let Some(ref local) = local_ip {
                        if ip == local {
                            tracing::info!("⏭️  [PHASE2-PEER] Skipping self-connection to {}", ip);
                            continue;
                        }
                    }

                    // Skip if this is a masternode (already connected in Phase 1)
                    if masternodes.iter().any(|mn| mn.masternode.address == *ip) {
                        continue;
                    }

                    if connection_manager.is_connected(ip).await {
                        tracing::debug!("Already connected to {}", ip);
                        continue;
                    }

                    if !connection_manager.mark_connecting(ip).await {
                        tracing::debug!("[PHASE2-PEER] Already connecting to {}, skipping", ip);
                        continue;
                    }

                    tracing::info!("🔗 [PHASE2-PEER] Connecting to: {}", ip);

                    spawn_connection_task(
                        ip.clone(),
                        p2p_port,
                        connection_manager.clone(),
                        masternode_registry.clone(),
                        blockchain.clone(),
                        attestation_system.clone(),
                        peer_manager.clone(),
                        peer_registry.clone(),
                        false, // regular peer
                        local_ip.clone(),
                    );

                    sleep(Duration::from_millis(100)).await;
                }
            }

            // PHASE 3: Periodic peer discovery with masternode priority
            let peer_discovery_interval = Duration::from_secs(120);
            loop {
                sleep(peer_discovery_interval).await;

                // Always check masternodes first
                let masternodes = masternode_registry.list_active().await;
                let connected_count = connection_manager.connected_count().await;

                tracing::info!(
                    "🔍 Peer check: {} connected, {} active masternodes, {} total slots",
                    connected_count,
                    masternodes.len(),
                    max_peers
                );

                // Reconnect to any disconnected masternodes (HIGH PRIORITY)
                for mn in masternodes.iter().take(reserved_masternode_slots) {
                    let ip = &mn.masternode.address;

                    // CRITICAL FIX: Skip if this is our own IP
                    if let Some(ref local) = local_ip {
                        if ip == local {
                            continue;
                        }
                    }

                    if !connection_manager.is_connected(ip).await
                        && connection_manager.mark_connecting(ip).await
                    {
                        tracing::info!(
                            "🎯 [PHASE3-MN-PRIORITY] Reconnecting to masternode: {}",
                            ip
                        );

                        spawn_connection_task(
                            ip.clone(),
                            p2p_port,
                            connection_manager.clone(),
                            masternode_registry.clone(),
                            blockchain.clone(),
                            attestation_system.clone(),
                            peer_manager.clone(),
                            peer_registry.clone(),
                            true,
                            local_ip.clone(),
                        );
                    }
                }

                // Fill any remaining slots with regular peers
                let available_slots = max_peers.saturating_sub(connected_count);
                if available_slots > 0 {
                    let current_peers = peer_manager.get_all_peers().await;

                    // Deduplicate peers by IP (remove port) to prevent duplicate connections
                    let mut seen_ips = std::collections::HashSet::new();
                    let unique_peers: Vec<String> = current_peers
                        .into_iter()
                        .filter_map(|peer_addr| {
                            let ip = if let Some(colon_pos) = peer_addr.rfind(':') {
                                &peer_addr[..colon_pos]
                            } else {
                                &peer_addr
                            };

                            // Only keep first occurrence of each IP
                            if seen_ips.insert(ip.to_string()) {
                                Some(ip.to_string())
                            } else {
                                None
                            }
                        })
                        .collect();

                    tracing::info!(
                        "🔗 {} connection slot(s) available, checking {} unique peer candidates",
                        available_slots,
                        unique_peers.len()
                    );

                    for ip in unique_peers.iter().take(available_slots) {
                        // CRITICAL FIX: Skip if this is our own IP
                        if let Some(ref local) = local_ip {
                            if ip == local {
                                continue;
                            }
                        }

                        // Skip masternodes (they're handled above with priority)
                        if masternodes.iter().any(|mn| mn.masternode.address == *ip) {
                            continue;
                        }

                        // Check if already connected OR already connecting (prevents race condition)
                        if connection_manager.is_connected(ip).await {
                            continue;
                        }

                        // Check if peer is in reconnection backoff - don't start duplicate connection
                        if connection_manager.is_reconnecting(ip).await {
                            continue;
                        }

                        // Atomically check and mark as connecting
                        if !connection_manager.mark_connecting(ip).await {
                            // Another task already connecting, skip
                            continue;
                        }

                        tracing::info!(
                            "🔗 [PHASE3-PEER] Discovered new peer, connecting to: {}",
                            ip
                        );

                        spawn_connection_task(
                            ip.clone(),
                            p2p_port,
                            connection_manager.clone(),
                            masternode_registry.clone(),
                            blockchain.clone(),
                            attestation_system.clone(),
                            peer_manager.clone(),
                            peer_registry.clone(),
                            false,
                            local_ip.clone(),
                        );

                        sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
    }
}

/// Helper function to spawn a persistent connection task
#[allow(clippy::too_many_arguments)]
fn spawn_connection_task(
    ip: String,
    port: u16,
    connection_manager: Arc<ConnectionManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    attestation_system: Arc<HeartbeatAttestationSystem>,
    peer_manager: Arc<PeerManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    is_masternode: bool,
    local_ip: Option<String>,
) {
    let tag = if is_masternode { "[MASTERNODE]" } else { "" };
    tracing::debug!("{} spawn_connection_task called for {}", tag, ip);

    tokio::spawn(async move {
        let mut retry_delay = 5;
        let mut consecutive_failures = 0;
        let max_failures = if is_masternode { 20 } else { 10 }; // Masternodes get more retries

        loop {
            match maintain_peer_connection(
                &ip,
                port,
                connection_manager.clone(),
                masternode_registry.clone(),
                blockchain.clone(),
                attestation_system.clone(),
                peer_manager.clone(),
                peer_registry.clone(),
                local_ip.clone(),
            )
            .await
            {
                Ok(_) => {
                    let tag = if is_masternode { "[MASTERNODE]" } else { "" };
                    tracing::info!("{} Connection to {} ended gracefully", tag, ip);
                    consecutive_failures = 0;
                    retry_delay = 5;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    let tag = if is_masternode { "[MASTERNODE]" } else { "" };
                    tracing::warn!(
                        "{} Connection to {} failed (attempt {}): {}",
                        tag,
                        ip,
                        consecutive_failures,
                        e
                    );

                    if consecutive_failures >= max_failures {
                        tracing::error!(
                            "{} Giving up on {} after {} failed attempts",
                            tag,
                            ip,
                            consecutive_failures
                        );
                        connection_manager.clear_reconnecting(&ip).await;
                        break;
                    }

                    retry_delay = (retry_delay * 2).min(300);
                }
            }

            connection_manager.mark_disconnected(&ip).await;

            let tag = if is_masternode { "[MASTERNODE]" } else { "" };
            tracing::info!("{} Reconnecting to {} in {}s...", tag, ip, retry_delay);

            // Mark peer as in reconnection backoff to prevent duplicate connection attempts
            connection_manager
                .mark_reconnecting(&ip, retry_delay, consecutive_failures)
                .await;

            sleep(Duration::from_secs(retry_delay)).await;

            // Clear reconnection state after backoff completes
            connection_manager.clear_reconnecting(&ip).await;

            // Check if already connected/connecting before reconnecting
            if connection_manager.is_connected(&ip).await {
                tracing::debug!(
                    "{} Already connected to {} during reconnect, task exiting",
                    tag,
                    ip
                );
                break;
            }

            if !connection_manager.mark_connecting(&ip).await {
                tracing::debug!(
                    "{} Already connecting to {} during reconnect, task exiting",
                    tag,
                    ip
                );
                break;
            }
        }

        connection_manager.mark_disconnected(&ip).await;
    });
}

/// Maintain a persistent connection to a peer
#[allow(clippy::too_many_arguments)]
async fn maintain_peer_connection(
    ip: &str,
    port: u16,
    connection_manager: Arc<ConnectionManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    attestation_system: Arc<HeartbeatAttestationSystem>,
    peer_manager: Arc<PeerManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    _local_ip: Option<String>,
) -> Result<(), String> {
    // Connect directly - connection manager just tracks we're connected
    let addr = format!("{}:{}", ip, port);
    let connection_start = std::time::Instant::now();
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    // Configure TCP socket options for persistent connections
    // Disable Nagle's algorithm to prevent batching of small messages
    if let Err(e) = stream.set_nodelay(true) {
        tracing::warn!("Failed to set TCP_NODELAY: {}", e);
    }

    // Enable TCP keepalive to detect dead connections
    // This prevents wasted connection slots from silently dead peers
    let socket = socket2::SockRef::from(&stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(std::time::Duration::from_secs(30)) // Send first probe after 30s idle
        .with_interval(std::time::Duration::from_secs(10)); // Send probes every 10s

    if let Err(e) = socket.set_tcp_keepalive(&keepalive) {
        tracing::warn!("Failed to set TCP_KEEPALIVE: {}", e);
    } else {
        tracing::debug!("✓ TCP keepalive enabled for {}", ip);
    }

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
    let msg_with_newline = format!("{}\n", handshake_json);
    writer
        .write_all(msg_with_newline.as_bytes())
        .await
        .map_err(|e| format!("Failed to send handshake: {}", e))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("Failed to flush handshake: {}", e))?;

    tracing::debug!("📡 Sent handshake to {}", ip);

    // Wait for handshake ACK before sending other messages
    let mut line = String::new();

    // Read until we get the handshake ACK or timeout after 10 seconds
    let ack_timeout = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    tracing::warn!(
                        "🔌 {} closed connection immediately after handshake (before ACK)",
                        ip
                    );
                    return Err("Connection closed before handshake ACK".to_string());
                }
                Ok(_) => {
                    if let Ok(msg) = serde_json::from_str::<NetworkMessage>(&line) {
                        if let NetworkMessage::Ack { message_type } = msg {
                            if message_type == "Handshake" {
                                tracing::debug!("✅ Received handshake ACK from {}", ip);
                                return Ok(());
                            }
                        } else {
                            // Got another message - store it for later processing if needed
                            tracing::debug!(
                                "📨 Received message before ACK from {}: {:?}",
                                ip,
                                std::mem::discriminant(&msg)
                            );
                        }
                    }
                }
                Err(e) => {
                    return Err(format!("Error reading handshake ACK: {}", e));
                }
            }
        }
    })
    .await;

    match ack_timeout {
        Ok(Ok(())) => {
            tracing::info!("🤝 Handshake completed with {}", ip);
            // Clear any reconnection backoff state since we successfully connected
            connection_manager.clear_reconnecting(ip).await;
        }
        Ok(Err(e)) => {
            return Err(format!("Handshake ACK failed: {}", e));
        }
        Err(_) => {
            tracing::warn!("⏱️  Handshake ACK timeout from {} - proceeding anyway", ip);
            // Continue anyway for backward compatibility with older nodes
            connection_manager.clear_reconnecting(ip).await;
        }
    }

    // Register writer with PeerConnectionRegistry
    // After this, all messages must be sent through the registry
    peer_registry.register_peer(ip.to_string(), writer).await;
    tracing::debug!("📝 Registered {} in PeerConnectionRegistry", ip);

    // Announce our masternode if we are one
    if let Some(local_mn) = masternode_registry.get_local_masternode().await {
        let announce_msg = NetworkMessage::MasternodeAnnouncement {
            address: local_mn.masternode.address.clone(),
            reward_address: local_mn.reward_address.clone(),
            tier: local_mn.masternode.tier,
            public_key: local_mn.masternode.public_key,
        };

        peer_registry
            .send_to_peer(ip, announce_msg)
            .await
            .map_err(|e| format!("Failed to send masternode announcement: {}", e))?;

        tracing::info!("📡 Announced masternode to {}", ip);

        // Small delay to ensure message is sent separately
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Send heartbeat and sync check every 60 seconds (blocks are every 10 minutes)
    let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(60));

    // Send ping for health check every 30 seconds
    let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
    let mut pending_ping: Option<(u64, std::time::Instant)> = None; // (nonce, sent_time)
    let mut consecutive_missed_pongs = 0u32;

    // Initial height request
    let sync_msg = NetworkMessage::GetBlockHeight;
    peer_registry
        .send_to_peer(ip, sync_msg)
        .await
        .map_err(|e| format!("Failed to send initial height request: {}", e))?;

    tracing::info!("📡 Requested initial block height from {}", ip);

    // Small delay to ensure message is sent separately
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Request pending transactions (to catch any we missed during downtime)
    let tx_request = NetworkMessage::GetPendingTransactions;
    peer_registry
        .send_to_peer(ip, tx_request)
        .await
        .map_err(|e| format!("Failed to send pending transactions request: {}", e))?;

    tracing::debug!("📡 Requested pending transactions from {}", ip);

    // Request masternode list
    let mn_request = NetworkMessage::GetMasternodes;
    peer_registry
        .send_to_peer(ip, mn_request)
        .await
        .map_err(|e| format!("Failed to send masternode request: {}", e))?;

    tracing::debug!("📡 Requested masternode list from {}", ip);

    // Request peer list for peer discovery
    let peers_request = NetworkMessage::GetPeers;
    peer_registry
        .send_to_peer(ip, peers_request)
        .await
        .map_err(|e| format!("Failed to send peers request: {}", e))?;

    tracing::debug!("📡 Requested peer list from {}", ip);

    // Read responses (reuse the line buffer from handshake)
    line.clear();
    tracing::info!(
        "🔄 Starting message loop for peer {} (connection established)",
        ip
    );

    loop {
        tokio::select! {
            // Send periodic ping for health check
            _ = ping_interval.tick() => {
                // Check if there's a pending ping that timed out (5 second timeout)
                if let Some((nonce, sent_time)) = pending_ping {
                    if sent_time.elapsed() > std::time::Duration::from_secs(5) {
                        consecutive_missed_pongs += 1;
                        tracing::warn!(
                            "⚠️ Ping timeout from {} (nonce: {}, missed: {}/3)",
                            ip, nonce, consecutive_missed_pongs
                        );

                        if consecutive_missed_pongs >= 3 {
                            tracing::error!(
                                "❌ Peer {} unresponsive after 3 missed pongs, disconnecting",
                                ip
                            );
                            break;
                        }
                    }
                }

                // Send new ping
                let nonce = rand::random::<u64>();
                let timestamp = chrono::Utc::now().timestamp();
                let ping_msg = NetworkMessage::Ping { nonce, timestamp };

                if let Err(e) = peer_registry.send_to_peer(ip, ping_msg).await {
                    tracing::warn!("❌ Failed to send ping to {}: {}", ip, e);
                    break;
                }

                pending_ping = Some((nonce, std::time::Instant::now()));
                tracing::debug!("📤 Sent ping to {} (nonce: {})", ip, nonce);
            }

            // Send periodic heartbeat and sync check
            _ = heartbeat_interval.tick() => {
                tracing::debug!("💓 Sending heartbeat/sync to {}", ip);

                // Send masternode announcement
                if let Some(local_mn) = masternode_registry.get_local_masternode().await {
                    let heartbeat_msg = NetworkMessage::MasternodeAnnouncement {
                        address: local_mn.masternode.address.clone(),
                        reward_address: local_mn.reward_address.clone(),
                        tier: local_mn.masternode.tier,
                        public_key: local_mn.masternode.public_key,
                    };
                    if let Err(e) = peer_registry.send_to_peer(ip, heartbeat_msg).await {
                        tracing::warn!("❌ Failed to send heartbeat to {}: {}", ip, e);
                        break;
                    }
                }

                // Request peer height for sync check
                let sync_msg = NetworkMessage::GetBlockHeight;
                if let Err(e) = peer_registry.send_to_peer(ip, sync_msg).await {
                    tracing::warn!("❌ Failed to send sync request to {}: {}", ip, e);
                    break;
                }

                // Request UTXO state hash for verification (every 10 minutes)
                let utxo_check_msg = NetworkMessage::GetUTXOStateHash;
                if let Err(e) = peer_registry.send_to_peer(ip, utxo_check_msg).await {
                    tracing::warn!("❌ Failed to send UTXO check to {}: {}", ip, e);
                    break;
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
                                NetworkMessage::Ack { message_type } => {
                                    tracing::debug!("✅ Received ACK for {} from {}", message_type, ip);
                                    // ACKs are informational, no action needed
                                }
                                NetworkMessage::BlockHeightResponse(remote_height) => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::BlockHeightResponse(remote_height)).await;

                                    let local_height = blockchain.get_height().await;
                                    tracing::info!("📊 Peer {} has height {}, we have {}", ip, remote_height, local_height);

                                    if remote_height > local_height {
                                        // Check if we're already syncing
                                        let is_syncing = blockchain.is_syncing().await;

                                        if !is_syncing {
                                            // Mark as syncing and request blocks from this peer
                                            blockchain.set_syncing(true).await;
                                            tracing::info!("📥 Peer {} has height {}, we have {}. Starting sync...", ip, remote_height, local_height);

                                            // If we have no blocks, start from genesis (block 0)
                                            let start_height = if local_height == 0 { 0 } else { local_height + 1 };
                                            let req = NetworkMessage::GetBlocks(start_height, remote_height);
                                            let _ = peer_registry.send_to_peer(ip, req).await;
                                        } else {
                                            tracing::debug!("Already syncing from another peer, skipping");
                                        }
                                    } else if remote_height == local_height {
                                        // We're synced, clear syncing flag
                                        blockchain.set_syncing(false).await;
                                        tracing::debug!("✅ Synced with peer {} at height {}", ip, local_height);
                                    } else {
                                        tracing::info!("📈 We have height {} which is ahead of peer {} at {}", local_height, ip, remote_height);
                                    }
                                }
                                NetworkMessage::BlocksResponse(blocks) => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::BlocksResponse(blocks.clone())).await;

                                    tracing::info!("📦 Received {} blocks from peer", blocks.len());

                                    let mut blocks_added = 0;
                                    let mut had_fork_error = false;

                                    for block in blocks {
                                        // Validate timestamp - block shouldn't be from the future
                                        let now = chrono::Utc::now().timestamp();
                                        let max_future_seconds = 600; // Allow 10 minutes tolerance for clock drift

                                        if block.header.timestamp > now + max_future_seconds {
                                            tracing::warn!(
                                                "⚠️ Rejecting block {} from future: timestamp {} is {}s ahead",
                                                block.header.height,
                                                block.header.timestamp,
                                                block.header.timestamp - now
                                            );
                                            continue;
                                        }

                                        match blockchain.add_block(block).await {
                                            Ok(_) => {
                                                blocks_added += 1;

                                                // Every 100 blocks, check network sync and request more if needed
                                                if blocks_added % 100 == 0 {
                                                    let current_height = blockchain.get_height().await;
                                                    tracing::info!("✅ Synced {} blocks, current height: {}", blocks_added, current_height);

                                                    // Request next batch if we're still behind
                                                    let sync_msg = NetworkMessage::GetBlockHeight;
                                                    let _ = peer_registry.send_to_peer(ip, sync_msg).await;
                                                }
                                            }
                                            Err(e) => {
                                                if e.to_string().contains("Fork detected") {
                                                    tracing::warn!("🍴 Fork detected while syncing: {}", e);
                                                    had_fork_error = true;
                                                    // Fork resolution is triggered automatically when blocks arrive
                                                    // Just stop processing this batch and re-check height
                                                    tracing::info!("🔄 Will re-check height after fork detection");
                                                    break;
                                                } else {
                                                    tracing::warn!("Failed to add block: {}", e);
                                                }
                                            }
                                        }
                                    }

                                    if blocks_added > 0 {
                                        tracing::info!("✅ Successfully added {} blocks", blocks_added);
                                    }

                                    // Clear syncing flag - either we succeeded or hit a fork that needs resolution
                                    blockchain.set_syncing(false).await;

                                    // If we hit a fork, request height again to check if we should sync differently
                                    if had_fork_error {
                                        tracing::info!("🔄 Fork detected during sync - requesting updated height to reassess");
                                        let sync_msg = NetworkMessage::GetBlockHeight;
                                        let _ = peer_registry.send_to_peer(ip, sync_msg).await;
                                    }
                                }
                                NetworkMessage::PendingTransactionsResponse(transactions) => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::PendingTransactionsResponse(transactions.clone())).await;

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
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::MasternodesResponse(masternodes.clone())).await;

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

                                            // Strip port from address to ensure consistency
                                            let ip_only = mn_data.address.split(':').next()
                                                .unwrap_or(&mn_data.address).to_string();

                                            let mn = crate::types::Masternode {
                                                address: ip_only,
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
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::UTXOStateHashResponse { hash, height, utxo_count }).await;

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
                                        let _ = peer_registry.send_to_peer(ip, request).await;
                                        tracing::info!("📥 Requesting full UTXO set from peer for reconciliation");
                                    } else if height == local_height {
                                        tracing::debug!("✅ UTXO state matches peer at height {}", height);
                                    }
                                }
                                NetworkMessage::UTXOSetResponse(utxos) => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::UTXOSetResponse(utxos.clone())).await;

                                    tracing::info!("📥 Received {} UTXOs from peer for reconciliation", utxos.len());
                                    blockchain.reconcile_utxo_state(utxos).await;
                                }
                                NetworkMessage::PeersResponse(peers) => {
                                    tracing::debug!("📩 Received peer list from {} with {} peer(s)", ip, peers.len());
                                    let mut added = 0;
                                    for peer_addr in peers {
                                        if peer_manager.add_peer_candidate(peer_addr.clone()).await {
                                            added += 1;
                                        }
                                    }
                                    if added > 0 {
                                        tracing::info!("✓ Added {} new peer candidate(s) from {}", added, ip);
                                    }
                                }
                                NetworkMessage::HeartbeatBroadcast(heartbeat) => {
                                    tracing::debug!("💓 Received heartbeat from {} seq {}",
                                        heartbeat.masternode_address, heartbeat.sequence_number);

                                    // Process heartbeat and create attestation if we're a masternode
                                    match attestation_system.receive_heartbeat(heartbeat.clone()).await {
                                        Ok(Some(attestation)) => {
                                            // We created an attestation, broadcast it back
                                            tracing::debug!("✍️ Broadcasting our attestation");
                                            // Forward to masternode registry for broadcast
                                            masternode_registry.broadcast_attestation(attestation).await;
                                        }
                                        Ok(None) => {
                                            tracing::debug!("✓ Processed heartbeat (no attestation needed)");
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to process heartbeat: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::HeartbeatAttestation(attestation) => {
                                    tracing::debug!("✍️ Received attestation from {} for heartbeat {}",
                                        attestation.witness_address,
                                        hex::encode(&attestation.heartbeat_hash[..8]));

                                    if let Err(e) = attestation_system.add_attestation(attestation).await {
                                        tracing::warn!("Failed to add attestation: {}", e);
                                    }
                                }
                                NetworkMessage::BlockHashResponse { height, hash } => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::BlockHashResponse { height, hash }).await;

                                    tracing::debug!("📥 Received BlockHashResponse for height {}", height);
                                    // Fork resolution logic would use this
                                    if let Some(our_hash) = blockchain.get_block_hash_at_height(height).await {
                                        if let Some(peer_hash) = hash {
                                            if our_hash != peer_hash {
                                                tracing::warn!("🍴 Fork detected at height {}: our hash {} vs peer hash {}",
                                                    height, hex::encode(our_hash), hex::encode(peer_hash));
                                            }
                                        }
                                    }
                                }
                                NetworkMessage::ConsensusQueryResponse { agrees, height, their_hash } => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::ConsensusQueryResponse { agrees, height, their_hash }).await;

                                    tracing::debug!("📥 Received ConsensusQueryResponse for height {}: agrees={}", height, agrees);
                                    if !agrees {
                                        tracing::warn!("⚠️ Peer disagrees on block hash at height {}", height);
                                        tracing::debug!("Peer's hash: {}", hex::encode(their_hash));
                                    }
                                }
                                NetworkMessage::BlockRangeResponse(blocks) => {
                                    // Route response to any waiting query
                                    peer_registry.handle_response(ip, NetworkMessage::BlockRangeResponse(blocks.clone())).await;

                                    tracing::info!("📦 Received block range: {} blocks from peer", blocks.len());
                                    // Process blocks for reorg
                                    for block in blocks {
                                        if let Err(e) = blockchain.add_block(block).await {
                                            tracing::warn!("Failed to add block from range: {}", e);
                                        }
                                    }
                                }
                                NetworkMessage::Ping { nonce, timestamp: _ } => {
                                    // Respond to ping with pong
                                    let pong_msg = NetworkMessage::Pong {
                                        nonce,
                                        timestamp: chrono::Utc::now().timestamp(),
                                    };

                                    if let Err(e) = peer_registry.send_to_peer(ip, pong_msg).await {
                                        tracing::warn!("❌ Failed to send pong to {}: {}", ip, e);
                                        break;
                                    }
                                    tracing::debug!("📤 Sent pong to {} (nonce: {})", ip, nonce);
                                }
                                NetworkMessage::Pong { nonce, timestamp: _ } => {
                                    // Check if this pong matches our pending ping
                                    if let Some((pending_nonce, sent_time)) = pending_ping {
                                        if nonce == pending_nonce {
                                            let rtt = sent_time.elapsed();
                                            tracing::debug!(
                                                "✅ Received pong from {} (nonce: {}, RTT: {}ms)",
                                                ip, nonce, rtt.as_millis()
                                            );
                                            pending_ping = None;
                                            consecutive_missed_pongs = 0; // Reset counter on successful pong
                                        } else {
                                            tracing::warn!(
                                                "⚠️ Received pong with wrong nonce from {} (expected: {}, got: {})",
                                                ip, pending_nonce, nonce
                                            );
                                        }
                                    } else {
                                        tracing::debug!("📥 Received unexpected pong from {} (no pending ping)", ip);
                                    }
                                }
                                _ => {}
                            }
                        }
                        line.clear();
                    }
                    Err(e) => {
                        tracing::info!("🔌 Connection to {} ended: {} (after {} seconds)", ip, e, connection_start.elapsed().as_secs());
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
