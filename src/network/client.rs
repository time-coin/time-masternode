use crate::blockchain::Blockchain;
use crate::heartbeat_attestation::HeartbeatAttestationSystem;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::network::peer_connection::{PeerConnection, PeerStateManager};
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::NetworkType;
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct NetworkClient {
    peer_manager: Arc<PeerManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    attestation_system: Arc<HeartbeatAttestationSystem>,
    peer_connection_registry: Arc<PeerConnectionRegistry>,
    peer_state: Arc<PeerStateManager>,
    connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    p2p_port: u16,
    max_peers: usize,
    reserved_masternode_slots: usize,
    local_ip: Option<String>,
}

impl NetworkClient {
    pub fn new(
        peer_manager: Arc<PeerManager>,
        masternode_registry: Arc<MasternodeRegistry>,
        blockchain: Arc<Blockchain>,
        attestation_system: Arc<HeartbeatAttestationSystem>,
        network_type: NetworkType,
        max_peers: usize,
        peer_connection_registry: Arc<PeerConnectionRegistry>,
        peer_state: Arc<PeerStateManager>,
        connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
        local_ip: Option<String>,
    ) -> Self {
        let reserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30);

        Self {
            peer_manager,
            masternode_registry,
            blockchain,
            attestation_system,
            peer_connection_registry,
            peer_state,
            connection_manager,
            p2p_port: network_type.default_p2p_port(),
            max_peers,
            reserved_masternode_slots,
            local_ip,
        }
    }

    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();
        let blockchain = self.blockchain.clone();
        let attestation_system = self.attestation_system.clone();
        let peer_registry = self.peer_connection_registry.clone();
        let _peer_state = self.peer_state.clone();
        let connection_manager = self.connection_manager.clone();
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

            // PHASE 1: Connect to all active masternodes FIRST (priority) - PARALLEL
            let masternodes = masternode_registry.list_active().await;
            tracing::info!(
                "🎯 Connecting to {} active masternode(s) with priority (parallel)...",
                masternodes.len()
            );

            const CONCURRENT_DIALS: usize = 10;
            const BURST_INTERVAL_MS: u64 = 50;

            let mut masternode_tasks = Vec::new();
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

                if connection_manager.is_connected(&ip) {
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
                let attest = attestation_system.clone();
                let peer_mgr = peer_manager.clone();
                let peer_reg = peer_registry.clone();
                let local_ip_clone = local_ip.clone();

                // Spawn task without waiting for it to complete
                let task = tokio::spawn(async move {
                    spawn_connection_task(
                        ip_clone,
                        p2p_port,
                        conn_mgr,
                        mn_reg,
                        bc,
                        attest,
                        peer_mgr,
                        peer_reg,
                        true, // is_masternode flag
                        local_ip_clone,
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
                    // CRITICAL FIX: Skip if this is our own IP
                    if let Some(ref local) = local_ip {
                        if ip == local {
                            tracing::info!("⏭️  [PHASE2-PEER] Skipping self-connection to {}", ip);
                            continue;
                        }

                        // CRITICAL FIX: Only connect if our IP < peer IP (deterministic direction)
                        if local.as_str() >= ip.as_str() {
                            tracing::debug!("⏸️  [PHASE2-PEER] Skipping outbound to {} (they should connect to us: {} >= {})", ip, local, ip);
                            continue;
                        }
                    }

                    // Skip if this is a masternode (already connected in Phase 1)
                    if masternodes.iter().any(|mn| mn.masternode.address == *ip) {
                        continue;
                    }

                    if connection_manager.is_connected(ip) {
                        tracing::debug!("Already connected to {}", ip);
                        continue;
                    }

                    if !connection_manager.mark_connecting(ip) {
                        tracing::debug!("[PHASE2-PEER] Already connecting to {}, skipping", ip);
                        continue;
                    }

                    tracing::info!("🔗 [PHASE2-PEER] Connecting to: {}", ip);

                    let ip_clone = ip.clone();
                    let conn_mgr = connection_manager.clone();
                    let mn_reg = masternode_registry.clone();
                    let bc = blockchain.clone();
                    let attest = attestation_system.clone();
                    let peer_mgr = peer_manager.clone();
                    let peer_reg = peer_registry.clone();
                    let local_ip_clone = local_ip.clone();

                    // Spawn task without waiting
                    let task = tokio::spawn(async move {
                        spawn_connection_task(
                            ip_clone,
                            p2p_port,
                            conn_mgr,
                            mn_reg,
                            bc,
                            attest,
                            peer_mgr,
                            peer_reg,
                            false, // regular peer
                            local_ip_clone,
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

                // Always check masternodes first
                let masternodes = masternode_registry.list_active().await;
                let connected_count = connection_manager.connected_count();

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

                        // CRITICAL FIX: Only connect if our IP < peer IP (deterministic direction)
                        if local.as_str() >= ip.as_str() {
                            tracing::debug!("⏸️  [PHASE3-MN-PRIORITY] Skipping outbound to {} (they should connect to us: {} >= {})", ip, local, ip);
                            continue;
                        }
                    }

                    if !connection_manager.is_connected(ip)
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

                            // CRITICAL FIX: Only connect if our IP < peer IP (deterministic direction)
                            if local.as_str() >= ip.as_str() {
                                tracing::debug!("⏸️  [PHASE3-PEER] Skipping outbound to {} (they should connect to us: {} >= {})", ip, local, ip);
                                continue;
                            }
                        }

                        // Skip masternodes (they're handled above with priority)
                        if masternodes.iter().any(|mn| mn.masternode.address == *ip) {
                            continue;
                        }

                        // Check if already connected OR already connecting (prevents race condition)
                        if connection_manager.is_connected(ip) {
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
                        connection_manager.clear_reconnecting(&ip);
                        break;
                    }

                    retry_delay = (retry_delay * 2).min(300);
                }
            }

            connection_manager.mark_disconnected(&ip);

            let tag = if is_masternode { "[MASTERNODE]" } else { "" };
            tracing::info!("{} Reconnecting to {} in {}s...", tag, ip, retry_delay);

            // Mark peer as in reconnection backoff to prevent duplicate connection attempts
            connection_manager.mark_reconnecting(
                &ip,
                std::time::Duration::from_secs(retry_delay),
                consecutive_failures,
            );

            sleep(Duration::from_secs(retry_delay)).await;

            // Clear reconnection state after backoff completes
            connection_manager.clear_reconnecting(&ip);

            // Check if already connected/connecting before reconnecting
            if connection_manager.is_connected(&ip) {
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
    });
}

/// Maintain a persistent connection to a peer
#[allow(clippy::too_many_arguments)]
async fn maintain_peer_connection(
    ip: &str,
    port: u16,
    connection_manager: Arc<ConnectionManager>,
    _masternode_registry: Arc<MasternodeRegistry>,
    _blockchain: Arc<Blockchain>,
    _attestation_system: Arc<HeartbeatAttestationSystem>,
    _peer_manager: Arc<PeerManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    _local_ip: Option<String>,
) -> Result<(), String> {
    // Create outbound connection
    let peer_conn = PeerConnection::new_outbound(ip.to_string(), port).await?;

    tracing::info!("✓ Connected to peer: {}", ip);

    // Get peer IP for later reference
    let peer_ip = peer_conn.peer_ip().to_string();

    // Run the message loop which handles ping/pong and routes other messages
    // Pass peer_registry so it can register incoming messages from this peer
    let result = peer_conn
        .run_message_loop_with_registry(peer_registry.clone())
        .await;

    // Clean up on disconnect
    connection_manager.mark_disconnected(&peer_ip);
    peer_registry.unregister_peer(&peer_ip).await;
    tracing::debug!("🔌 Unregistered peer {}", peer_ip);

    result
}
