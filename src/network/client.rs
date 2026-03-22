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
use crate::network::blacklist::IPBlacklist;
use crate::network::connection_manager::ConnectionManager;
use crate::network::peer_connection::{PeerConnection, PeerStateManager};
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::network::tls::TlsConfig;
use crate::peer_manager::PeerManager;
use crate::NetworkType;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
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
    /// Real-time blacklist for rejecting messages from banned peers
    ip_blacklist: Option<Arc<RwLock<IPBlacklist>>>,
    /// AI-powered adaptive reconnection
    reconnection_ai: Arc<AdaptiveReconnectionAI>,
    /// TLS configuration for encrypted connections
    tls_config: Option<Arc<TlsConfig>>,
    /// Network type (mainnet/testnet)
    network_type: NetworkType,
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
        ip_blacklist: Option<Arc<RwLock<IPBlacklist>>>,
    ) -> Self {
        let reserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30);

        // Default AI-powered reconnection system (immediately overridden by set_reconnection_ai in main.rs).
        // Use an ephemeral DB for this throwaway instance — it is never actually queried.
        let ephemeral_db = Arc::new(
            sled::Config::new()
                .temporary(true)
                .open()
                .expect("ephemeral sled DB for NetworkClient default reconnection AI"),
        );
        let reconnection_ai = Arc::new(AdaptiveReconnectionAI::new(
            ephemeral_db,
            ReconnectionConfig::default(),
        ));

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
            ip_blacklist,
            reconnection_ai,
            tls_config: None,
            network_type,
        }
    }

    /// Replace the reconnection AI with a shared instance from AISystem.
    /// This ensures connection learning data is shared across all subsystems.
    pub fn set_reconnection_ai(&mut self, ai: Arc<AdaptiveReconnectionAI>) {
        self.reconnection_ai = ai;
    }

    /// Set TLS configuration for encrypted peer connections
    pub fn set_tls_config(&mut self, tls_config: Arc<TlsConfig>) {
        self.tls_config = Some(tls_config);
    }

    pub async fn start(&self) {
        let peer_manager = self.peer_manager.clone();
        let masternode_registry = self.masternode_registry.clone();
        let blockchain = self.blockchain.clone();
        let peer_registry = self.peer_connection_registry.clone();
        let connection_manager = self.connection_manager.clone();
        let max_peers = self.max_peers;
        let reserved_masternode_slots = self.reserved_masternode_slots;
        let local_ip = self.local_ip.clone();
        let blacklisted_peers = self.blacklisted_peers.clone();

        let res = ConnectionResources {
            port: self.p2p_port,
            connection_manager: connection_manager.clone(),
            masternode_registry: masternode_registry.clone(),
            blockchain: blockchain.clone(),
            peer_manager: peer_manager.clone(),
            peer_registry: peer_registry.clone(),
            local_ip: local_ip.clone(),
            reconnection_ai: self.reconnection_ai.clone(),
            ip_blacklist: self.ip_blacklist.clone(),
            tls_config: self.tls_config.clone(),
            network_type: self.network_type,
        };

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

            // Helper: should we skip connecting to this IP?
            let should_skip = |ip: &str| -> bool {
                if let Some(ref local) = local_ip {
                    if ip == local.as_str() {
                        return true;
                    }
                }
                if blacklisted_peers.contains(ip) {
                    return true;
                }
                // Skip if already connected, connecting, or reconnecting
                if connection_manager.is_active(ip) || peer_registry.is_connected(ip) {
                    return true;
                }
                false
            };

            // Helper: deduplicate peer addresses by IP
            let dedup_peers = |peers: Vec<String>| -> Vec<String> {
                let mut seen = std::collections::HashSet::new();
                peers
                    .into_iter()
                    .filter_map(|addr| {
                        let ip = if let Some(pos) = addr.rfind(':') {
                            &addr[..pos]
                        } else {
                            &addr
                        };
                        if seen.insert(ip.to_string()) {
                            Some(ip.to_string())
                        } else {
                            None
                        }
                    })
                    .collect()
            };

            // PHASE 1: Pyramid-aware startup connections.
            //
            // Network topology mirrors the collateral tier hierarchy:
            //
            //          ┌──── Gold ────┐   ← full mesh backbone (few nodes, high stake)
            //         Silver ── Silver      ← connect ALL Gold + lateral Silver peers
            //        Bronze  ── Bronze      ← connect N Silver (upward) + lateral Bronze
            //       Free  Free  Free  Free  ← connect N Bronze/Silver (upward only)
            //
            // SMALL NETWORK EXCEPTION: if total masternodes ≤ connection limit, every
            // node connects to every other node (full mesh regardless of tier).  This
            // guarantees all nodes see each other for gossip, voting, and rewards.
            use crate::types::MasternodeTier;
            use rand::seq::SliceRandom;

            // Number of peers to connect to per relationship
            const GOLD_SILVER_EXTRAS: usize = 3; // Gold also connects to N Silver for downward visibility
            const SILVER_LATERAL: usize = 4; // Silver lateral peers within Silver tier
            const BRONZE_UPWARD: usize = 5; // Bronze → Silver connections
            const BRONZE_LATERAL: usize = 3; // Bronze lateral peers within Bronze tier
            const FREE_UPWARD: usize = 5; // Free → Bronze connections (+ 1 Silver fallback)
            const FULL_MESH_THRESHOLD: usize = 20; // Use full mesh when total nodes ≤ this

            // Determine our own tier
            let our_tier: Option<MasternodeTier> = {
                let our_ip = masternode_registry.get_local_address().await;
                match our_ip {
                    Some(ref ip) => masternode_registry.get(ip).await.map(|i| i.masternode.tier),
                    None => None,
                }
            };

            // Fetch masternodes by tier once
            let gold_nodes = masternode_registry.list_by_tier(MasternodeTier::Gold).await;
            let mut silver_nodes = masternode_registry
                .list_by_tier(MasternodeTier::Silver)
                .await;
            let mut bronze_nodes = masternode_registry
                .list_by_tier(MasternodeTier::Bronze)
                .await;
            let mut free_nodes = masternode_registry.list_by_tier(MasternodeTier::Free).await;

            silver_nodes.shuffle(&mut rand::thread_rng());
            bronze_nodes.shuffle(&mut rand::thread_rng());
            free_nodes.shuffle(&mut rand::thread_rng());

            let total_masternodes =
                gold_nodes.len() + silver_nodes.len() + bronze_nodes.len() + free_nodes.len();

            // Build the connection target list for our tier
            let targets: Vec<String> = if total_masternodes <= FULL_MESH_THRESHOLD {
                // Small network: connect to everyone — full mesh guarantees all nodes
                // can gossip, vote, and see each other regardless of tier.
                tracing::info!(
                    "🔗 [PHASE1] Small network ({} masternodes ≤ {}): using full mesh",
                    total_masternodes,
                    FULL_MESH_THRESHOLD
                );
                gold_nodes
                    .iter()
                    .chain(silver_nodes.iter())
                    .chain(bronze_nodes.iter())
                    .chain(free_nodes.iter())
                    .map(|m| m.masternode.address.clone())
                    .collect()
            } else {
                match our_tier {
                    Some(MasternodeTier::Gold) => {
                        // Gold: full mesh with ALL Gold + a few Silver for downward visibility
                        let mut t: Vec<String> = gold_nodes
                            .iter()
                            .map(|m| m.masternode.address.clone())
                            .collect();
                        t.extend(
                            silver_nodes
                                .iter()
                                .take(GOLD_SILVER_EXTRAS)
                                .map(|m| m.masternode.address.clone()),
                        );
                        t
                    }
                    Some(MasternodeTier::Silver) => {
                        // Silver: connect to ALL Gold (backbone) + lateral Silver peers
                        let mut t: Vec<String> = gold_nodes
                            .iter()
                            .map(|m| m.masternode.address.clone())
                            .collect();
                        t.extend(
                            silver_nodes
                                .iter()
                                .take(SILVER_LATERAL)
                                .map(|m| m.masternode.address.clone()),
                        );
                        t
                    }
                    Some(MasternodeTier::Bronze) => {
                        // Bronze: N Silver (upward) + lateral Bronze peers; fall back to Gold if no Silver
                        let mut t: Vec<String> = silver_nodes
                            .iter()
                            .take(BRONZE_UPWARD)
                            .map(|m| m.masternode.address.clone())
                            .collect();
                        if t.is_empty() {
                            t.extend(gold_nodes.iter().map(|m| m.masternode.address.clone()));
                        }
                        t.extend(
                            bronze_nodes
                                .iter()
                                .take(BRONZE_LATERAL)
                                .map(|m| m.masternode.address.clone()),
                        );
                        t
                    }
                    None | Some(MasternodeTier::Free) => {
                        // Free / unregistered: connect upward to Bronze, with a Silver fallback
                        let mut t: Vec<String> = bronze_nodes
                            .iter()
                            .take(FREE_UPWARD)
                            .map(|m| m.masternode.address.clone())
                            .collect();
                        t.extend(
                            silver_nodes
                                .iter()
                                .take(1)
                                .map(|m| m.masternode.address.clone()),
                        );
                        if t.is_empty() {
                            // Last resort: any Gold that is reachable
                            t.extend(gold_nodes.iter().map(|m| m.masternode.address.clone()));
                        }
                        t
                    }
                }
            };

            tracing::info!(
                "🔺 [PHASE1] Pyramid startup (our tier: {:?}) — {} target(s): {:?}",
                our_tier,
                targets.len(),
                targets
            );

            let mut masternode_connections = 0;
            for ip in targets.iter().take(reserved_masternode_slots) {
                if should_skip(ip) {
                    if connection_manager.is_connected(ip) || peer_registry.is_connected(ip) {
                        masternode_connections += 1;
                    }
                    continue;
                }
                if !connection_manager.mark_connecting(ip) {
                    continue;
                }
                masternode_connections += 1;
                tracing::info!("🔗 [PHASE1] Connecting to: {} (tier: {:?})", ip, our_tier);
                res.spawn(ip.clone(), true);
            }

            // Brief delay for masternode connections to initiate
            sleep(Duration::from_millis(500)).await;

            tracing::info!(
                "✅ Initiated {} masternode connection(s), {} slots for regular peers",
                masternode_connections,
                max_peers.saturating_sub(masternode_connections)
            );

            // PHASE 2: Fill remaining slots with regular peers
            let available_slots = max_peers.saturating_sub(masternode_connections);
            if available_slots > 0 {
                let unique_peers = dedup_peers(peer_manager.get_all_peers().await);
                tracing::info!(
                    "🔌 Filling {} slot(s) with {} unique regular peers",
                    available_slots,
                    unique_peers.len()
                );

                for ip in unique_peers.iter().take(available_slots) {
                    if should_skip(ip) {
                        continue;
                    }
                    if !connection_manager.mark_connecting(ip) {
                        continue;
                    }
                    let is_registered_mn = masternode_registry
                        .list_all()
                        .await
                        .iter()
                        .any(|mn| mn.masternode.address == *ip);
                    tracing::info!("🔗 [PHASE2] Connecting to: {}", ip);
                    res.spawn(ip.clone(), is_registered_mn);
                }
            }

            // PHASE 3: Periodic peer discovery with masternode priority
            let peer_discovery_interval = Duration::from_secs(30);
            loop {
                sleep(peer_discovery_interval).await;

                // Clean up stale Connecting states (stuck >30s)
                let stale = connection_manager.cleanup_stale_connecting(Duration::from_secs(30));
                if stale > 0 {
                    tracing::info!("🧹 Reset {} stale connecting peer(s)", stale);
                }

                let active_count = masternode_registry.list_active().await.len();
                let outbound_count = connection_manager.connected_count();
                let inbound_count = peer_registry.inbound_count();

                tracing::debug!(
                    "🔍 Peer check: {} connected ({} out, {} in), {} active masternodes, {} total slots",
                    outbound_count + inbound_count,
                    outbound_count,
                    inbound_count,
                    active_count,
                    max_peers
                );

                // Masternodes: ensure full mesh with all registered masternodes.
                // Phase 1 handles initial connections, but masternodes that come
                // online after our startup (or that we lost connection to) must
                // be reconnected here. Uses list_all() (not list_active) because
                // masternodes marked inactive are exactly the ones we need to
                // reconnect to. AI reconnection advice still applies to avoid
                // hammering nodes that are legitimately offline.
                {
                    let all_masternodes = masternode_registry.list_all().await;
                    let total_mn = all_masternodes.len();
                    let mut reconnected = 0usize;

                    for mn_info in &all_masternodes {
                        let mn_ip = &mn_info.masternode.address;
                        if should_skip(mn_ip) {
                            continue;
                        }
                        if connection_manager.is_reconnecting(mn_ip) {
                            continue;
                        }
                        // Respect AI advice to avoid hammering offline nodes
                        let advice = res.reconnection_ai.get_reconnection_advice(mn_ip, true);
                        if !advice.should_attempt {
                            tracing::debug!(
                                "⏭️  [PHASE3-MN] Skipping {} (AI cooldown: {})",
                                mn_ip,
                                advice.reasoning
                            );
                            continue;
                        }
                        if !connection_manager.mark_connecting(mn_ip) {
                            continue;
                        }
                        tracing::info!(
                            "🔗 [PHASE3-MN] Reconnecting to masternode {} (tier: {:?})",
                            mn_ip,
                            mn_info.masternode.tier
                        );
                        res.spawn(mn_ip.clone(), true);
                        reconnected += 1;
                        sleep(Duration::from_millis(100)).await;
                    }

                    if reconnected > 0 {
                        tracing::info!(
                            "🔗 [PHASE3-MN] Initiated {} masternode reconnection(s) ({} registered)",
                            reconnected, total_mn
                        );
                    } else if total_mn > 1 {
                        tracing::debug!(
                            "🔗 [PHASE3-MN] All {} registered masternodes already connected or skipped",
                            total_mn
                        );
                    }
                }

                // Fill remaining slots with regular peers — prefer less-loaded ones
                // so new nodes naturally spread connections across the network.
                let available_slots = max_peers.saturating_sub(outbound_count + inbound_count);
                if available_slots > 0 {
                    let mut unique_peers = dedup_peers(peer_manager.get_all_peers().await);
                    // Sort by known connection load (ascending) so we dial the least-loaded
                    // candidates first.  Peers with unknown load sort to the back (u16::MAX).
                    unique_peers.sort_by_key(|ip| peer_registry.get_peer_load(ip));
                    for ip in unique_peers.iter().take(available_slots) {
                        if should_skip(ip) {
                            continue;
                        }
                        // Skip masternodes — handled by Phase 3-MN block above
                        if masternode_registry.get(ip).await.is_some() {
                            continue;
                        }
                        if connection_manager.is_reconnecting(ip) {
                            continue;
                        }
                        // Check AI advice before spawning. If a peer has failed enough
                        // times to reach deep exponential backoff (≥5 consecutive
                        // failures), evict it from the peer_manager entirely — it
                        // will be re-added via PeerExchange if it recovers.
                        const FORGET_THRESHOLD: u32 = 5;
                        let failures = res.reconnection_ai.consecutive_failures_for(ip);
                        if failures >= FORGET_THRESHOLD {
                            peer_manager.remove_peer(ip).await;
                            res.reconnection_ai.forget_peer(ip);
                            tracing::info!(
                                "🗑️  Evicted persistently unreachable peer {} ({} consecutive failures)",
                                ip, failures
                            );
                            continue;
                        }
                        let advice = res.reconnection_ai.get_reconnection_advice(ip, false);
                        if !advice.should_attempt {
                            tracing::debug!(
                                "⏭️  [PHASE3-PEER] Skipping {} (AI cooldown: {})",
                                ip,
                                advice.reasoning
                            );
                            continue;
                        }
                        if !connection_manager.mark_connecting(ip) {
                            continue;
                        }
                        tracing::info!("🔗 [PHASE3-PEER] Connecting to: {}", ip);
                        res.spawn(ip.clone(), false);
                        sleep(Duration::from_millis(100)).await;
                    }
                }

                // PHASE 4: Periodic chain tip comparison for fork detection
                let our_height = blockchain.get_height();
                if our_height > 0 {
                    let our_hash = blockchain.get_block_hash(our_height).unwrap_or([0u8; 32]);
                    let connected_peers = peer_registry.get_connected_peers().await;
                    if !connected_peers.is_empty() {
                        tracing::debug!(
                            "🔍 Chain tip check: height {} hash {}, querying {} peers",
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

/// Shared resources for spawning peer connections.
/// Eliminates repeated Arc cloning at each call site.
#[derive(Clone)]
struct ConnectionResources {
    port: u16,
    connection_manager: Arc<ConnectionManager>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
    peer_manager: Arc<PeerManager>,
    peer_registry: Arc<PeerConnectionRegistry>,
    local_ip: Option<String>,
    reconnection_ai: Arc<AdaptiveReconnectionAI>,
    ip_blacklist: Option<Arc<RwLock<IPBlacklist>>>,
    tls_config: Option<Arc<TlsConfig>>,
    network_type: NetworkType,
}

impl ConnectionResources {
    /// Spawn a one-shot connection task for a peer.
    /// Reconnection is handled externally by the Phase 3 discovery loop (every 120s),
    /// which re-spawns tasks for any masternode still in the registry.
    fn spawn(&self, ip: String, is_masternode: bool) {
        let res = self.clone();
        let tag = if is_masternode { "[MASTERNODE]" } else { "" };
        tracing::debug!("{} spawn_connection_task called for {}", tag, ip);

        tokio::spawn(async move {
            // Check if AI advises skipping this peer entirely
            let advice = res
                .reconnection_ai
                .get_reconnection_advice(&ip, is_masternode);
            if !advice.should_attempt {
                tracing::info!(
                    "🧠 [AI] Skipping connection to {}: {}",
                    ip,
                    advice.reasoning
                );
                res.connection_manager.clear_reconnecting(&ip);
                return;
            }

            let connect_start = std::time::Instant::now();
            match maintain_peer_connection(
                &ip,
                res.port,
                res.connection_manager.clone(),
                res.masternode_registry.clone(),
                res.blockchain.clone(),
                res.peer_manager.clone(),
                res.peer_registry.clone(),
                res.local_ip.clone(),
                is_masternode,
                res.ip_blacklist.clone(),
                res.tls_config.clone(),
                res.network_type,
            )
            .await
            {
                Ok(_) => {
                    let connect_time = connect_start.elapsed().as_millis() as u64;
                    res.reconnection_ai
                        .record_connection_success(&ip, is_masternode, connect_time);
                    tracing::info!("{} Connection to {} ended gracefully", tag, ip);
                }
                Err(e) => {
                    res.reconnection_ai
                        .record_connection_failure(&ip, is_masternode, &e);
                    tracing::warn!("{} Connection to {} failed: {}", tag, ip, e);
                }
            }

            res.connection_manager.mark_disconnected(&ip);

            // Mark inactive on disconnect (only if no live inbound connection replaced it)
            if is_masternode && !res.peer_registry.is_connected(&ip) {
                if let Err(e) = res
                    .masternode_registry
                    .mark_inactive_on_disconnect(&ip)
                    .await
                {
                    tracing::debug!("Could not mark masternode {} as inactive: {:?}", ip, e);
                }

                // If the node was removed (Free/Handshake tier), also remove it from the
                // peer list so it doesn't re-appear as a regular peer in the Phase 3 loop.
                if res.masternode_registry.get(&ip).await.is_none() {
                    res.peer_manager.remove_peer(&ip).await;
                }
            }
            // Task exits here. If this node is still in the registry (OnChain tier),
            // the Phase 3 loop will re-spawn a connection attempt every 120 seconds.
        });
    }
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
    ip_blacklist: Option<Arc<RwLock<IPBlacklist>>>,
    tls_config: Option<Arc<TlsConfig>>,
    network_type: NetworkType,
) -> Result<(), String> {
    // Mark in peer_registry BEFORE attempting connection to prevent race with inbound
    if !peer_registry.mark_connecting(ip) {
        return Err(format!(
            "Already connecting/connected to {} in peer_registry",
            ip
        ));
    }

    // Create outbound connection with whitelist status
    let peer_conn = match PeerConnection::new_outbound(
        ip.to_string(),
        port,
        is_masternode,
        tls_config,
        network_type,
    )
    .await
    {
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
        tracing::debug!(
            "🛡️ Marked {} as whitelisted masternode with enhanced protection",
            peer_ip
        );
    }

    // Run the message loop which handles ping/pong and routes other messages
    // Use the new unified message loop with builder pattern
    let mut config = crate::network::peer_connection::MessageLoopConfig::new(peer_registry.clone())
        .with_masternode_registry(masternode_registry.clone())
        .with_blockchain(blockchain.clone());

    // Add blacklist for message filtering
    if let Some(blacklist) = ip_blacklist {
        config = config.with_blacklist(blacklist);
    }

    // Subscribe to broadcast channel if available
    if let (_, _, Some(broadcast_tx)) = peer_registry.get_timelock_resources().await {
        let broadcast_rx = broadcast_tx.subscribe();
        config = config.with_broadcast_rx(broadcast_rx);
    }

    let result = peer_conn.run_message_loop_unified(config).await;

    // Clean up on disconnect in both managers
    connection_manager.mark_disconnected(&peer_ip);
    // Only clean up peer_registry if we're still the active outbound connection.
    // If the connection was superseded by an inbound (IP tiebreaker in
    // try_register_inbound), skip cleanup to avoid corrupting the new inbound.
    if peer_registry.is_outbound(&peer_ip) {
        peer_registry.mark_disconnected(&peer_ip);
        peer_registry.unregister_peer(&peer_ip).await;
    } else if !peer_registry.is_connected(&peer_ip) {
        // No connection at all — clean up normally
        peer_registry.unregister_peer(&peer_ip).await;
    } else {
        tracing::info!(
            "🔄 Outbound to {} superseded by inbound — skipping cleanup",
            peer_ip
        );
    }

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
