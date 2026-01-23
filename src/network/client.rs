//! Alternative network client implementation.
//!
//! Note: This module provides a structured NetworkClient abstraction but is
//! currently unused. Connection management is handled directly in main.rs
//! with more fine-grained control over the connection lifecycle.
//!
//! This implementation could be used in the future if we want to:
//! - Encapsulate all outbound connection logic in one place
//! - Provide a cleaner API for connection management
//! - Simplify main.rs by delegating to NetworkClient

#![allow(dead_code)]

use crate::ai::adaptive_reconnection::{AdaptiveReconnectionAI, ReconnectionConfig};
use crate::blockchain::Blockchain;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::network::peer_connection::{PeerConnection, PeerStateManager};
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::NetworkType;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct NetworkClient {
    peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    peer_connection_registry: Arc<PeerConnectionRegistry>,
    peer_state: Arc<PeerStateManager>,
    connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    p2p_port: u16,
    max_peers: usize,
    reserved_masternode_slots: usize,
    local_ip: Option<String>,
    blacklisted_peers: HashSet<String>,
    /// AI-powered adaptive reconnection
    reconnection_ai: Arc<AdaptiveReconnectionAI>,
}

impl NetworkClient {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        peer_manager: Arc<PeerManager>,
        masternode_registry: Arc<MasternodeRegistry>,
        blockchain: Arc<Blockchain>,
        network_type: NetworkType,
        max_peers: usize,
        peer_connection_registry: Arc<PeerConnectionRegistry>,
        peer_state: Arc<PeerStateManager>,
        connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
        local_ip: Option<String>,
        blacklisted_peers: Vec<String>,
    ) -> Self {
        let reserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30);

        // Initialize AI-powered reconnection system
        let reconnection_ai = Arc::new(AdaptiveReconnectionAI::new(ReconnectionConfig::default()));

        Self {
            peer_manager,
            masternode_registry,
            blockchain,
            peer_connection_registry,
            peer_state,
            connection_manager,
            p2p_port: network_type.default_p2p_port(),
            max_peers,
            reserved_masternode_slots,
            local_ip,
            blacklisted_peers: blacklisted_peers.into_iter().collect(),
            reconnection_ai,
        }
    }

    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();
        let blockchain = self.blockchain.clone();
        let peer_registry = self.peer_connection_registry.clone();
        let _peer_state = self.peer_state.clone();
        let connection_manager = self.connection_manager.clone();
        let p2p_port = self.p2p_port;
        let max_peers = self.max_peers;
        let reserved_masternode_slots = self.reserved_masternode_slots;
        let local_ip = self.local_ip.clone();
        let blacklisted_peers = self.blacklisted_peers.clone();
        let reconnection_ai = self.reconnection_ai.clone();

        tokio::spawn(async move {
            tracing::info!(
                "🔌 Starting network client (max peers: {}, reserved for masternodes: {})",
                max_peers,
                reserved_masternode_slots
            );
            tracing::info!("🧠 AI-powered adaptive reconnection enabled");

            if let Some(ref ip) = local_ip {
                tracing::info!("🏠 Local IP: {} (will skip self-connections)", ip);
            }

            // PHASE 1: Connect to all registered masternodes FIRST (priority) - PARALLEL
            // Use list_all() to include offline masternodes - they may come online
            let masternodes = masternode_registry.list_all().await;
            let masternode_ips: Vec<&str> = masternodes
                .iter()
                .map(|m| m.masternode.address.as_str())
                .collect();
            tracing::info!(
                "🎯 Connecting to {} registered masternode(s) with priority (parallel): {:?}",
                masternodes.len(),
                masternode_ips
            );

            const CONCURRENT_DIALS: usize = 10;
            const BURST_INTERVAL_MS: u64 = 50;

            let mut masternode_tasks = Vec::new();
            let mut masternode_connections = 0;

            for mn in masternodes.iter().take(reserved_masternode_slots) {
                let ip = mn.masternode.address.clone();

                // Skip if this is our own IP
                if let Some(ref local) = local_ip {
                    if ip == *local {
                        tracing::info!("⏭️  [PHASE1-MN] Skipping self-connection to {}", ip);
                        continue;
                    }
                }

                // Skip blacklisted peers
                if blacklisted_peers.contains(&ip) {
                    tracing::debug!("🚫 [PHASE1-MN] Skipping blacklisted peer: {}", ip);
                    continue;
                }

                tracing::info!("🔗 [PHASE1-MN] Initiating priority connection to: {}", ip);

                // Check if already connected in connection_manager OR peer_registry
                if connection_manager.is_connected(&ip) || peer_registry.is_connected(&ip) {
                    tracing::debug!("Already connected to masternode {}", ip);
                    masternode_connections += 1;
                    continue;
                }

                if !connection_manager.mark_connecting(&ip) {
                    tracing::debug!("[PHASE1-MN] Already connecting to {}, skipping", ip);
                    continue;
                }

                masternode_connections += 1;

                let ip_clone = ip.clone();
                let conn_mgr = connection_manager.clone();
                let mn_reg = masternode_registry.clone();
                let bc = blockchain.clone();
                let peer_mgr = peer_manager.clone();
                let peer_reg = peer_registry.clone();
                let local_ip_clone = local_ip.clone();
                let recon_ai = reconnection_ai.clone();

                // Spawn task without waiting for it to complete
                let task = tokio::spawn(async move {
                    spawn_connection_task(
                        ip_clone,
                        p2p_port,
                        conn_mgr,
                        mn_reg,
                        bc,
                        peer_mgr,
                        peer_reg,
                        true, // is_masternode flag
                        local_ip_clone,
                        recon_ai,
                    );
                });

                masternode_tasks.push(task);

                // Burst control: limit concurrent dials
                if masternode_tasks.len() >= CONCURRENT_DIALS {
                    sleep(Duration::from_millis(BURST_INTERVAL_MS)).await;
                }
            }

            // Wait for all masternode connections to initiate
            let start_time = std::time::Instant::now();
            for task in masternode_tasks {
                let _ = task.await;
            }
            let elapsed = start_time.elapsed();
            tracing::info!(
                "✅ Connected to {} masternode(s) in {:.2}s, {} slots available for regular peers",
                masternode_connections,
                elapsed.as_secs_f64(),
                max_peers.saturating_sub(masternode_connections)
            );

            // PHASE 2: Fill remaining slots with regular peers - PARALLEL
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
                    "🔌 Filling {} remaining slot(s) with {} unique regular peers (parallel)",
                    available_slots,
                    unique_peers.len()
                );

                let mut peer_tasks = Vec::new();

                for ip in unique_peers.iter().take(available_slots) {
                    // Skip if this is our own IP
                    if let Some(ref local) = local_ip {
                        if ip == local {
                            tracing::info!("⏭️  [PHASE2-PEER] Skipping self-connection to {}", ip);
                            continue;
                        }
                    }

                    // Skip blacklisted peers
                    if blacklisted_peers.contains(ip) {
                        tracing::debug!("🚫 [PHASE2-PEER] Skipping blacklisted peer: {}", ip);
                        continue;
                    }

                    // Skip if this is a masternode (already connected in Phase 1)
                    if masternodes.iter().any(|mn| mn.masternode.address == *ip) {
                        continue;
                    }

                    // Check if this IP is a registered masternode (even if not active)
                    // This ensures masternodes always get whitelist protection
                    let all_masternodes = masternode_registry.list_all().await;
                    let is_registered_masternode = all_masternodes
                        .iter()
                        .any(|mn| mn.masternode.address == *ip);

                    if connection_manager.is_connected(ip) || peer_registry.is_connected(ip) {
                        tracing::debug!("Already connected to {}", ip);
                        continue;
                    }

                    if !connection_manager.mark_connecting(ip) {
                        tracing::debug!("[PHASE2-PEER] Already connecting to {}, skipping", ip);
                        continue;
                    }

                    if is_registered_masternode {
                        tracing::info!(
                            "🔗 [PHASE2-MN-LATE] Connecting to registered masternode: {}",
                            ip
                        );
                    } else {
                        tracing::info!("🔗 [PHASE2-PEER] Connecting to: {}", ip);
                    }

                    let ip_clone = ip.clone();
                    let conn_mgr = connection_manager.clone();
                    let mn_reg = masternode_registry.clone();
                    let bc = blockchain.clone();
                    let peer_mgr = peer_manager.clone();
                    let peer_reg = peer_registry.clone();
                    let local_ip_clone = local_ip.clone();
                    let recon_ai = reconnection_ai.clone();

                    // Spawn task without waiting
                    let task = tokio::spawn(async move {
                        spawn_connection_task(
                            ip_clone,
                            p2p_port,
                            conn_mgr,
                            mn_reg,
                            bc,
                            peer_mgr,
                            peer_reg,
                            is_registered_masternode, // treat registered masternodes as whitelisted
                            local_ip_clone,
                            recon_ai,
                        );
                    });

                    peer_tasks.push(task);

                    // Burst control: limit concurrent dials
                    if peer_tasks.len() >= CONCURRENT_DIALS {
                        sleep(Duration::from_millis(BURST_INTERVAL_MS)).await;
                    }
                }

                // Wait for all peer connections to initiate
                let start_time = std::time::Instant::now();
                for task in peer_tasks {
                    let _ = task.await;
                }
                let elapsed = start_time.elapsed();
                tracing::info!(
                    "✅ Regular peer connections initiated in {:.2}s",
                    elapsed.as_secs_f64()
                );
            }

            // PHASE 3: Periodic peer discovery with masternode priority
            let peer_discovery_interval = Duration::from_secs(120);
            loop {
                sleep(peer_discovery_interval).await;

                // Use list_all() to get ALL known masternodes, not just active ones
                // This ensures we attempt to reconnect to masternodes that went offline
                // (their status will be restored once we reconnect and receive heartbeats)
                let all_masternodes = masternode_registry.list_all().await;
                let active_count = masternode_registry.list_active().await.len();
                let connected_count = connection_manager.connected_count();

                tracing::info!(
                    "🔍 Peer check: {} connected, {} known masternodes ({} active), {} total slots",
                    connected_count,
                    all_masternodes.len(),
                    active_count,
                    max_peers
                );

                // Reconnect to any disconnected masternodes (HIGH PRIORITY)
                // Note: For masternodes, we want BOTH nodes to establish outbound connections
                // to ensure full mesh redundancy. Only check if we have an outbound connection,
                // so both nodes will attempt outbound connections to each other.
                for mn in all_masternodes.iter().take(reserved_masternode_slots) {
                    let ip = &mn.masternode.address;

                    // Skip if this is our own IP
                    if let Some(ref local) = local_ip {
                        if ip == local {
                            continue;
                        }
                    }

                    // Skip blacklisted peers
                    if blacklisted_peers.contains(ip) {
                        continue;
                    }

                    // For masternodes: only skip if we already have an OUTBOUND connection
                    // This allows both nodes to connect outbound to each other for full mesh
                    if !connection_manager.has_outbound_connection(ip)
                        && connection_manager.mark_connecting(ip)
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
                            peer_manager.clone(),
                            peer_registry.clone(),
                            true,
                            local_ip.clone(),
                            reconnection_ai.clone(),
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
                        // Skip if this is our own IP
                        if let Some(ref local) = local_ip {
                            if ip == local {
                                continue;
                            }
                        }

                        // Skip blacklisted peers
                        if blacklisted_peers.contains(ip) {
                            continue;
                        }

                        // Skip masternodes (they're handled above with priority)
                        if masternodes.iter().any(|mn| mn.masternode.address == *ip) {
                            continue;
                        }

                        // Check if already connected OR already connecting (prevents race condition)
                        if connection_manager.is_connected(ip) || peer_registry.is_connected(ip) {
                            continue;
                        }

                        // Check if peer is in reconnection backoff - don't start duplicate connection
                        if connection_manager.is_reconnecting(ip) {
                            continue;
                        }

                        // Atomically check and mark as connecting
                        if !connection_manager.mark_connecting(ip) {
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
                            peer_manager.clone(),
                            peer_registry.clone(),
                            false,
                            local_ip.clone(),
                            reconnection_ai.clone(),
                        );

                        sleep(Duration::from_millis(100)).await;
                    }
                }

                // PHASE 4: Periodic chain tip comparison for fork detection
                // Query all connected peers for their chain tip and check for forks
                let our_height = blockchain.get_height();
                if our_height > 0 {
                    let our_hash = blockchain.get_block_hash(our_height).unwrap_or([0u8; 32]);

                    // Send GetChainTip to all connected peers
                    let connected_peers = peer_registry.get_connected_peers().await;
                    if !connected_peers.is_empty() {
                        tracing::debug!(
                            "🔍 Chain tip check: our height {} hash {}, querying {} peers",
                            our_height,
                            hex::encode(&our_hash[..8]),
                            connected_peers.len()
                        );

                        for peer_ip in connected_peers.iter() {
                            let msg = crate::network::message::NetworkMessage::GetChainTip;
                            if let Err(e) = peer_registry.send_to_peer(peer_ip, msg).await {
                                tracing::debug!("Failed to send GetChainTip to {}: {}", peer_ip, e);
                            }
                        }
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
    peer_manager: Arc<PeerManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    is_masternode: bool,
    local_ip: Option<String>,
    reconnection_ai: Arc<AdaptiveReconnectionAI>,
) {
    let tag = if is_masternode { "[MASTERNODE]" } else { "" };
    tracing::debug!("{} spawn_connection_task called for {}", tag, ip);

    tokio::spawn(async move {
        let mut session_start: Option<std::time::Instant> = None;

        loop {
            // Get AI-powered reconnection advice
            let advice = reconnection_ai.get_reconnection_advice(&ip, is_masternode);

            if !advice.should_attempt {
                tracing::info!(
                    "🧠 [AI] Skipping reconnection to {}: {}",
                    ip,
                    advice.reasoning
                );
                connection_manager.clear_reconnecting(&ip);
                break;
            }

            let connect_start = std::time::Instant::now();

            match maintain_peer_connection(
                &ip,
                port,
                connection_manager.clone(),
                masternode_registry.clone(),
                blockchain.clone(),
                peer_manager.clone(),
                peer_registry.clone(),
                local_ip.clone(),
                is_masternode,
            )
            .await
            {
                Ok(_) => {
                    let connect_time = connect_start.elapsed().as_millis() as u64;
                    reconnection_ai.record_connection_success(&ip, is_masternode, connect_time);

                    // Record session duration if we had a session
                    if let Some(start) = session_start {
                        reconnection_ai.record_session_end(&ip, start.elapsed().as_secs());
                    }
                    session_start = Some(std::time::Instant::now());

                    let tag = if is_masternode { "[MASTERNODE]" } else { "" };
                    tracing::info!("{} Connection to {} ended gracefully", tag, ip);
                }
                Err(e) => {
                    reconnection_ai.record_connection_failure(&ip, is_masternode, &e);

                    // Record session duration if we had a session
                    if let Some(start) = session_start {
                        reconnection_ai.record_session_end(&ip, start.elapsed().as_secs());
                    }
                    session_start = None;

                    let tag = if is_masternode { "[MASTERNODE]" } else { "" };
                    tracing::warn!("{} Connection to {} failed: {}", tag, ip, e);
                }
            }

            connection_manager.mark_disconnected(&ip);

            // If this is a masternode connection, mark it as inactive in the registry
            if is_masternode {
                if let Err(e) = masternode_registry.mark_inactive_on_disconnect(&ip).await {
                    tracing::debug!("Could not mark masternode {} as inactive: {:?}", ip, e);
                }
            }

            // Get updated advice after recording the result
            let advice = reconnection_ai.get_reconnection_advice(&ip, is_masternode);
            let retry_delay = advice.delay_secs;

            let tag = if is_masternode { "[MASTERNODE]" } else { "" };
            tracing::info!(
                "{} 🧠 [AI] Reconnecting to {} in {}s (priority={:?})",
                tag,
                ip,
                retry_delay,
                advice.priority
            );

            // Mark peer as in reconnection backoff
            connection_manager.mark_reconnecting(
                &ip,
                std::time::Duration::from_secs(retry_delay),
                0, // AI handles failure tracking internally
            );

            sleep(Duration::from_secs(retry_delay)).await;

            // Clear reconnection state after backoff completes
            connection_manager.clear_reconnecting(&ip);

            // Check if already connected/connecting before reconnecting
            if connection_manager.is_connected(&ip) || peer_registry.is_connected(&ip) {
                tracing::debug!(
                    "{} Already connected to {} during reconnect, task exiting",
                    tag,
                    ip
                );
                break;
            }

            if !connection_manager.mark_connecting(&ip) {
                tracing::debug!(
                    "{} Already connecting to {} during reconnect, task exiting",
                    tag,
                    ip
                );
                break;
            }
        }

        connection_manager.mark_disconnected(&ip);

        // Final cleanup: Mark masternode as inactive when task exits
        if is_masternode {
            if let Err(e) = masternode_registry.mark_inactive_on_disconnect(&ip).await {
                tracing::debug!(
                    "Could not mark masternode {} as inactive on task exit: {:?}",
                    ip,
                    e
                );
            }
        }
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
    _peer_manager: Arc<PeerManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    _local_ip: Option<String>,
    is_masternode: bool,
) -> Result<(), String> {
    // Mark in peer_registry BEFORE attempting connection to prevent race with inbound
    if !peer_registry.mark_connecting(ip) {
        return Err(format!(
            "Already connecting/connected to {} in peer_registry",
            ip
        ));
    }

    // Create outbound connection with whitelist status
    let peer_conn = match PeerConnection::new_outbound(ip.to_string(), port, is_masternode).await {
        Ok(conn) => conn,
        Err(e) => {
            // Failed to connect - clean up peer_registry mark
            peer_registry.unregister_peer(ip).await;
            return Err(e);
        }
    };

    tracing::info!("✓ Connected to peer: {}", ip);

    // Get peer IP for later reference
    let peer_ip = peer_conn.peer_ip().to_string();

    // Mark as connected in connection_manager (transitions from Connecting -> Connected)
    connection_manager.mark_connected(&peer_ip);

    // Phase 2: Mark whitelisted masternodes in connection_manager for protection
    if is_masternode {
        connection_manager.mark_whitelisted(&peer_ip);
        tracing::info!(
            "🛡️ Marked {} as whitelisted masternode with enhanced protection",
            peer_ip
        );
    }

    // Run the message loop which handles ping/pong and routes other messages
    // Use the new unified message loop with builder pattern
    let mut config = crate::network::peer_connection::MessageLoopConfig::new(peer_registry.clone())
        .with_masternode_registry(masternode_registry.clone())
        .with_blockchain(blockchain.clone());

    // Subscribe to broadcast channel if available
    if let (_, _, Some(broadcast_tx)) = peer_registry.get_tsdc_resources().await {
        let broadcast_rx = broadcast_tx.subscribe();
        config = config.with_broadcast_rx(broadcast_rx);
    }

    let result = peer_conn.run_message_loop_unified(config).await;

    // Clean up on disconnect in both managers
    connection_manager.mark_disconnected(&peer_ip);
    peer_registry.mark_inbound_disconnected(&peer_ip);
    peer_registry.unregister_peer(&peer_ip).await;

    // If this peer is a registered masternode, mark it as inactive on disconnect
    if masternode_registry.is_registered(&peer_ip).await {
        if let Err(e) = masternode_registry
            .mark_inactive_on_disconnect(&peer_ip)
            .await
        {
            tracing::debug!("Could not mark masternode {} as inactive: {:?}", peer_ip, e);
        }
    }

    tracing::debug!("🔌 Unregistered peer {}", peer_ip);

    result
}
