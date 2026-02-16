//! Unified message handler for both inbound and outbound connections
//!
//! This module provides a single implementation of network message handling
//! that works regardless of connection direction. Previously, message handling
//! was duplicated between server.rs (inbound) and peer_connection.rs (outbound).

use crate::block::types::calculate_merkle_root;
use crate::block::types::Block;
use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::blacklist::IPBlacklist;
use crate::network::dedup_filter::DeduplicationFilter;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::types::{OutPoint, UTXOState}; // Add explicit imports
use crate::utxo_manager::UTXOStateManager;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

/// Direction of the network connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    Inbound,
    Outbound,
}

impl std::fmt::Display for ConnectionDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionDirection::Inbound => write!(f, "Inbound"),
            ConnectionDirection::Outbound => write!(f, "Outbound"),
        }
    }
}

/// Context containing all dependencies needed for message handling
pub struct MessageContext {
    pub blockchain: Arc<Blockchain>,
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub masternode_registry: Arc<MasternodeRegistry>,
    pub consensus: Option<Arc<ConsensusEngine>>,
    pub block_cache: Option<Arc<crate::network::block_cache::BlockCache>>,
    pub broadcast_tx: Option<broadcast::Sender<NetworkMessage>>,
    // Extended context for full message handling
    pub utxo_manager: Option<Arc<UTXOStateManager>>,
    pub peer_manager: Option<Arc<PeerManager>>,
    pub seen_blocks: Option<Arc<DeduplicationFilter>>,
    pub seen_transactions: Option<Arc<DeduplicationFilter>>,
    // Node identity for voting
    pub node_masternode_address: Option<String>,
    // Blacklist for rejecting messages from banned peers
    pub blacklist: Option<Arc<RwLock<IPBlacklist>>>,
    // AI System for recording events and making intelligent decisions
    pub ai_system: Option<Arc<crate::ai::AISystem>>,
}

impl MessageContext {
    /// Create a minimal context with only required fields
    pub fn minimal(
        blockchain: Arc<Blockchain>,
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Self {
        Self {
            blockchain,
            peer_registry,
            masternode_registry,
            consensus: None,
            block_cache: None,
            broadcast_tx: None,
            utxo_manager: None,
            peer_manager: None,
            seen_blocks: None,
            seen_transactions: None,
            node_masternode_address: None,
            blacklist: None,
            ai_system: None,
        }
    }

    /// Create context with consensus resources for transaction/block handling
    pub fn with_consensus(
        blockchain: Arc<Blockchain>,
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
        consensus: Arc<ConsensusEngine>,
        block_cache: Arc<crate::network::block_cache::BlockCache>,
        broadcast_tx: broadcast::Sender<NetworkMessage>,
        node_masternode_address: Option<String>,
    ) -> Self {
        Self {
            blockchain,
            peer_registry,
            masternode_registry,
            consensus: Some(consensus),
            block_cache: Some(block_cache),
            broadcast_tx: Some(broadcast_tx),
            utxo_manager: None,
            peer_manager: None,
            seen_blocks: None,
            seen_transactions: None,
            node_masternode_address,
            blacklist: None,
            ai_system: None,
        }
    }

    /// Create context and automatically fetch consensus resources from peer registry
    /// This is the preferred method for creating MessageContext as it ensures
    /// consensus engine is available for block/vote handling
    pub async fn from_registry(
        blockchain: Arc<Blockchain>,
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Self {
        // Fetch consensus resources from peer registry
        let (consensus, block_cache, broadcast_tx) = peer_registry.get_timelock_resources().await;
        // Get local masternode address for voting identity
        let node_masternode_address = masternode_registry.get_local_address().await;
        // Get AI system from blockchain if available
        let ai_system = blockchain.ai_system().cloned();

        // Populate utxo_manager from consensus engine if available
        let utxo_manager = consensus.as_ref().map(|c| Arc::clone(&c.utxo_manager));

        Self {
            blockchain,
            peer_registry,
            masternode_registry,
            consensus,
            block_cache,
            broadcast_tx,
            utxo_manager,
            peer_manager: None,
            seen_blocks: None,
            seen_transactions: None,
            node_masternode_address,
            blacklist: None,
            ai_system,
        }
    }

    /// Set the node's masternode address for voting identity
    pub fn with_node_address(mut self, address: Option<String>) -> Self {
        self.node_masternode_address = address;
        self
    }

    /// Set the blacklist for rejecting messages from banned peers
    pub fn with_blacklist(mut self, blacklist: Arc<RwLock<IPBlacklist>>) -> Self {
        self.blacklist = Some(blacklist);
        self
    }

    /// Set the AI system for intelligent event recording and decision making
    pub fn with_ai_system(mut self, ai_system: Arc<crate::ai::AISystem>) -> Self {
        self.ai_system = Some(ai_system);
        self
    }
}

/// Tracks repeated GetBlocks requests to detect loops
#[derive(Debug, Clone)]
struct GetBlocksRequest {
    start: u64,
    end: u64,
    timestamp: Instant,
}

/// Unified message handler for all network messages
pub struct MessageHandler {
    peer_ip: String,
    direction: ConnectionDirection,
    recent_requests: Arc<RwLock<Vec<GetBlocksRequest>>>,
    /// Tracks the last time a fork warning was logged for this peer (rate limiting)
    last_fork_warning: std::sync::Mutex<Option<Instant>>,
}

impl MessageHandler {
    /// Create a new message handler for a specific peer and connection direction
    pub fn new(peer_ip: String, direction: ConnectionDirection) -> Self {
        Self {
            peer_ip,
            direction,
            recent_requests: Arc::new(RwLock::new(Vec::new())),
            last_fork_warning: std::sync::Mutex::new(None),
        }
    }

    /// Get voter weight from masternode registry, defaulting to 1 if not found
    async fn get_voter_weight(registry: &MasternodeRegistry, voter_id: &str) -> u64 {
        match registry.get(voter_id).await {
            Some(info) => info.masternode.tier.sampling_weight().max(1),
            None => 1u64,
        }
    }

    /// Verify a vote signature (PREPARE or PRECOMMIT)
    /// Returns Ok(true) if valid, Ok(false) if invalid/rejected, Err on missing signature with backward compat
    async fn verify_vote_signature(
        &self,
        registry: &MasternodeRegistry,
        block_hash: &[u8; 32],
        voter_id: &str,
        vote_type: &[u8], // b"PREPARE" or b"PRECOMMIT"
        signature: &[u8],
    ) -> Result<bool, ()> {
        if signature.is_empty() {
            debug!(
                "[{}] Accepting unsigned {} vote from {} (backward compatibility)",
                self.direction,
                String::from_utf8_lossy(vote_type),
                voter_id
            );
            return Ok(true); // Accept unsigned for backward compatibility
        }

        let Some(info) = registry.get(voter_id).await else {
            debug!(
                "[{}] Cannot verify signature for unknown voter {}",
                self.direction, voter_id
            );
            return Ok(true); // Accept if we don't know the voter
        };

        use ed25519_dalek::{Signature, Verifier};

        // Reconstruct the signed message
        let mut msg = Vec::new();
        msg.extend_from_slice(block_hash);
        msg.extend_from_slice(voter_id.as_bytes());
        msg.extend_from_slice(vote_type);

        // Parse signature
        let sig_array: [u8; 64] = match signature.try_into() {
            Ok(arr) => arr,
            Err(_) => {
                warn!(
                    "❌ [{}] Invalid {} signature length from {} (expected 64 bytes, got {})",
                    self.direction,
                    String::from_utf8_lossy(vote_type),
                    voter_id,
                    signature.len()
                );
                return Ok(false); // Reject
            }
        };

        let sig = Signature::from_bytes(&sig_array);
        if let Err(e) = info.masternode.public_key.verify(&msg, &sig) {
            warn!(
                "❌ [{}] Invalid {} vote signature from {}: {}",
                self.direction,
                String::from_utf8_lossy(vote_type),
                voter_id,
                e
            );
            return Ok(false); // Reject
        }

        Ok(true) // Valid signature
    }

    /// Handle a network message and optionally return a response message
    ///
    /// # Arguments
    /// * `msg` - The message to handle
    /// * `context` - Shared context with blockchain, registries, etc.
    ///
    /// # Returns
    /// * `Ok(Some(response))` - Message handled successfully, send this response
    /// * `Ok(None)` - Message handled successfully, no response needed
    /// * `Err(msg)` - Error handling message
    pub async fn handle_message(
        &self,
        msg: &NetworkMessage,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // SECURITY: Check blacklist before processing ANY message
        if let Some(blacklist) = &context.blacklist {
            if let Ok(ip) = self.peer_ip.parse::<IpAddr>() {
                let mut bl = blacklist.write().await;
                if let Some(reason) = bl.is_blacklisted(ip) {
                    warn!(
                        "🚫 [{:?}] REJECTING message from blacklisted peer {}: {}",
                        self.direction, self.peer_ip, reason
                    );
                    return Err(format!("Peer {} is blacklisted: {}", self.peer_ip, reason));
                }
            }
        }

        let result = match msg {
            // === Health Check Messages ===
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => self.handle_ping(*nonce, *timestamp, *height, context).await,
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => self.handle_pong(*nonce, *timestamp, *height, context).await,

            // === Block Sync Messages ===
            NetworkMessage::GetBlocks(start, end) => {
                self.handle_get_blocks(*start, *end, context).await
            }
            NetworkMessage::GetBlockHeight => self.handle_get_block_height(context).await,
            NetworkMessage::GetChainTip => self.handle_get_chain_tip(context).await,
            NetworkMessage::GetBlockRange {
                start_height,
                end_height,
            } => {
                self.handle_get_block_range(*start_height, *end_height, context)
                    .await
            }
            NetworkMessage::GetBlockHash(height) => {
                self.handle_get_block_hash(*height, context).await
            }
            NetworkMessage::BlockRequest(height) => {
                self.handle_block_request(*height, context).await
            }
            NetworkMessage::BlockInventory(height) => {
                self.handle_block_inventory(*height, context).await
            }
            NetworkMessage::BlockResponse(block) => {
                self.handle_block_response(block.clone(), context).await
            }
            NetworkMessage::BlockAnnouncement(block) => {
                self.handle_block_announcement(block.clone(), context).await
            }

            // === Genesis Messages ===
            NetworkMessage::GetGenesisHash => self.handle_get_genesis_hash(context).await,
            NetworkMessage::GenesisHashResponse(hash) => {
                self.handle_genesis_hash_response(*hash, context).await
            }
            NetworkMessage::RequestGenesis => self.handle_request_genesis(context).await,
            NetworkMessage::GenesisAnnouncement(block) => {
                self.handle_genesis_announcement(block.clone(), context)
                    .await
            }

            // === Transaction Messages ===
            NetworkMessage::TransactionBroadcast(tx) => {
                self.handle_transaction_broadcast(tx.clone(), context).await
            }

            // === Peer Exchange Messages ===
            NetworkMessage::GetPeers => self.handle_get_peers(context).await,
            NetworkMessage::PeersResponse(peers) => {
                self.handle_peers_response(peers.clone(), context).await
            }

            // === Masternode Messages ===
            NetworkMessage::GetMasternodes => self.handle_get_masternodes(context).await,
            NetworkMessage::MasternodeAnnouncement { .. } => {
                // V1 deprecated — all nodes use V2 now
                debug!(
                    "⏭️  [{}] Ignoring deprecated V1 masternode announcement from {}",
                    self.direction, self.peer_ip
                );
                Ok(None)
            }
            NetworkMessage::MasternodeAnnouncementV2 {
                address,
                reward_address,
                tier,
                public_key,
                collateral_outpoint,
            } => {
                self.handle_masternode_announcement(
                    address.clone(),
                    reward_address.clone(),
                    *tier,
                    *public_key,
                    collateral_outpoint.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::MasternodeUnlock {
                address,
                collateral_outpoint,
                timestamp,
            } => {
                self.handle_masternode_unlock(
                    address.clone(),
                    collateral_outpoint.clone(),
                    *timestamp,
                    context,
                )
                .await
            }
            NetworkMessage::MasternodesResponse(masternodes) => {
                self.handle_masternodes_response(masternodes.clone(), context)
                    .await
            }
            NetworkMessage::MasternodeInactive { address, timestamp } => {
                self.handle_masternode_inactive(address.clone(), *timestamp, context)
                    .await
            }
            NetworkMessage::GetLockedCollaterals => {
                self.handle_get_locked_collaterals(context).await
            }
            NetworkMessage::LockedCollateralsResponse(collaterals) => {
                self.handle_locked_collaterals_response(collaterals.clone(), context)
                    .await
            }

            // === UTXO Messages ===
            NetworkMessage::UTXOStateQuery(outpoints) => {
                self.handle_utxo_state_query(outpoints.clone(), context)
                    .await
            }
            NetworkMessage::UTXOStateUpdate { outpoint, state } => {
                self.handle_utxo_state_update(outpoint.clone(), state.clone(), context)
                    .await
            }
            NetworkMessage::GetUTXOStateHash => self.handle_get_utxo_state_hash(context).await,
            NetworkMessage::GetUTXOSet => self.handle_get_utxo_set(context).await,

            // === Consensus Query Messages ===
            NetworkMessage::ConsensusQuery { height, block_hash } => {
                self.handle_consensus_query(*height, *block_hash, context)
                    .await
            }
            NetworkMessage::GetChainWork => self.handle_get_chain_work(context).await,
            NetworkMessage::GetChainWorkAt(height) => {
                self.handle_get_chain_work_at(*height, context).await
            }

            // === TimeLock Consensus Messages ===
            NetworkMessage::TimeLockBlockProposal { block } => {
                self.handle_timelock_block_proposal(block.clone(), context)
                    .await
            }
            NetworkMessage::TimeVotePrepare {
                block_hash,
                voter_id,
                signature,
            } => {
                self.handle_timelock_prepare_vote(
                    *block_hash,
                    voter_id.clone(),
                    signature.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::TimeVotePrecommit {
                block_hash,
                voter_id,
                signature,
            } => {
                self.handle_timelock_precommit_vote(
                    *block_hash,
                    voter_id.clone(),
                    signature.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::FinalityVoteBroadcast { vote } => {
                self.handle_finality_vote_broadcast(vote.clone(), context)
                    .await
            }

            // === §7.6 Liveness Fallback Protocol Messages ===
            NetworkMessage::LivenessAlert { alert } => {
                self.handle_liveness_alert(alert.clone(), context).await
            }
            NetworkMessage::FinalityProposal { proposal } => {
                self.handle_finality_proposal(proposal.clone(), context)
                    .await
            }
            NetworkMessage::FallbackVote { vote } => {
                self.handle_fallback_vote(vote.clone(), context).await
            }

            // === TimeVote Consensus Messages (§7 Transaction Finality) ===
            NetworkMessage::TimeVoteRequest {
                txid,
                tx_hash_commitment,
                slot_index,
                tx,
            } => {
                self.handle_timevote_request(
                    *txid,
                    *tx_hash_commitment,
                    *slot_index,
                    tx.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::TimeVoteResponse { vote } => {
                self.handle_timevote_response(vote.clone(), context).await
            }
            NetworkMessage::TimeProofBroadcast { proof } => {
                self.handle_timeproof_broadcast(proof.clone(), context)
                    .await
            }

            // === Gossip-based Status Tracking ===
            NetworkMessage::MasternodeStatusGossip {
                reporter,
                visible_masternodes,
                timestamp,
            } => {
                tracing::debug!(
                    "📥 [{:?}] Processing gossip from {}: {} masternodes visible",
                    self.direction,
                    reporter,
                    visible_masternodes.len()
                );
                context
                    .masternode_registry
                    .process_status_gossip(
                        reporter.clone(),
                        visible_masternodes.clone(),
                        *timestamp,
                    )
                    .await;
                Ok(None)
            }

            // === Fork Alert ===
            NetworkMessage::ForkAlert {
                your_height,
                your_hash,
                consensus_height,
                consensus_hash,
                consensus_peer_count,
                message,
            } => {
                self.handle_fork_alert(
                    *your_height,
                    *your_hash,
                    *consensus_height,
                    *consensus_hash,
                    *consensus_peer_count,
                    message.clone(),
                )
                .await
            }

            // === Chain Synchronization Response Messages ===
            NetworkMessage::ChainTipResponse { height, hash } => {
                self.handle_chain_tip_response(*height, *hash, context)
                    .await
            }
            NetworkMessage::BlocksResponse(blocks) | NetworkMessage::BlockRangeResponse(blocks) => {
                self.handle_blocks_response(blocks.clone(), context).await
            }

            // === Other Response Messages (handled by caller) ===
            NetworkMessage::BlockHeightResponse(_)
            | NetworkMessage::BlockHashResponse { .. }
            | NetworkMessage::UTXOStateResponse(_)
            | NetworkMessage::UTXOSetResponse(_)
            | NetworkMessage::UTXOStateHashResponse { .. }
            | NetworkMessage::ConsensusQueryResponse { .. }
            | NetworkMessage::ChainWorkResponse { .. }
            | NetworkMessage::ChainWorkAtResponse { .. }
            | NetworkMessage::PendingTransactionsResponse(_) => {
                // Response messages - no further action needed in handler
                Ok(None)
            }

            // === Messages not handled here ===
            _ => {
                debug!(
                    "[{}] Unhandled message type {:?} from {}",
                    self.direction,
                    msg.message_type(),
                    self.peer_ip
                );
                Ok(None)
            }
        };

        // Record AI events based on message processing results
        if let Some(ai) = &context.ai_system {
            // Record all messages as anomaly detector events (for traffic pattern analysis)
            ai.anomaly_detector
                .record_event(format!("msg_{}", msg.message_type()), 1.0);

            // Record errors as potential attack indicators
            if result.is_err() {
                ai.attack_detector.record_invalid_message(&self.peer_ip);
                ai.anomaly_detector
                    .record_event("invalid_message".to_string(), 1.0);
            }
        }

        result
    }

    /// Handle Ping message - respond with Pong
    async fn handle_ping(
        &self,
        nonce: u64,
        _timestamp: i64,
        peer_height: Option<u64>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📨 [{}] Received ping from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        // Update peer height if provided
        if let Some(h) = peer_height {
            context
                .peer_registry
                .update_peer_height(&self.peer_ip, h)
                .await;
        }

        // Include our height in pong response
        let our_height = context.blockchain.get_height();
        let pong = NetworkMessage::Pong {
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            height: Some(our_height),
        };

        debug!(
            "✅ [{}] Sent pong to {} (nonce: {}, height: {})",
            self.direction, self.peer_ip, nonce, our_height
        );

        Ok(Some(pong))
    }

    /// Handle Pong message - update peer height
    async fn handle_pong(
        &self,
        nonce: u64,
        _timestamp: i64,
        peer_height: Option<u64>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📨 [{}] Received pong from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        // Update peer height if provided
        if let Some(h) = peer_height {
            context
                .peer_registry
                .update_peer_height(&self.peer_ip, h)
                .await;
        }

        Ok(None)
    }

    /// Handle GetBlocks request - respond with BlocksResponse
    async fn handle_get_blocks(
        &self,
        start: u64,
        end: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_height = context.blockchain.get_height();
        debug!(
            "[{}] GetBlocks({}-{}) from {} (our height: {})",
            self.direction, start, end, self.peer_ip, our_height
        );

        // Check for repeated requests (loop detection)
        {
            let mut requests = self.recent_requests.write().await;
            let now = Instant::now();

            // Clean old requests (older than 30 seconds)
            requests.retain(|req| now.duration_since(req.timestamp) < Duration::from_secs(30));

            // Count similar requests in the last 30 seconds
            let similar_count = requests
                .iter()
                .filter(|req| {
                    // Consider requests similar if they overlap significantly
                    let start_match = (req.start as i64 - start as i64).abs() <= 100;
                    let end_match = (req.end as i64 - end as i64).abs() <= 100;
                    start_match && end_match
                })
                .count();

            if similar_count >= 20 {
                warn!(
                    "🚨 [{}] Possible sync loop detected: {} sent {} similar GetBlocks requests in 30s (ranges near {}-{}). Ignoring this request.",
                    self.direction, self.peer_ip, similar_count, start, end
                );
                // Return empty response to break the loop
                return Ok(Some(NetworkMessage::BlocksResponse(vec![])));
            }

            // Record this request
            requests.push(GetBlocksRequest {
                start,
                end,
                timestamp: now,
            });
        }

        let mut blocks = Vec::new();
        // Send blocks we have: cap at our_height, requested end, and batch limit of 100
        let effective_end = end
            .min(start + crate::constants::network::SYNC_BATCH_SIZE - 1)
            .min(our_height);

        if start <= our_height {
            // CRITICAL: Only send contiguous blocks starting from requested start
            // Stop at first missing block to avoid sending incomplete ranges with gaps
            for h in start..=effective_end {
                match context.blockchain.get_block_by_height(h).await {
                    Ok(block) => blocks.push(block),
                    Err(e) => {
                        // Stop at first missing block - don't send partial ranges with gaps
                        warn!(
                            "⚠️ [{}] Missing block {} (stopping send to {} at height {}): {}",
                            self.direction,
                            h,
                            self.peer_ip,
                            h.saturating_sub(1),
                            e
                        );
                        break;
                    }
                }
            }

            if blocks.is_empty() && start <= our_height {
                warn!(
                    "⚠️ [{}] No blocks available to send to {} (requested {}-{}, our height: {}, missing block {})",
                    self.direction, self.peer_ip, start, end, our_height, start
                );
            } else if !blocks.is_empty() {
                let actual_start = blocks.first().unwrap().header.height;
                let actual_end = blocks.last().unwrap().header.height;
                debug!(
                    "📤 [{}] Sending {} blocks to {} (requested {}-{}, sending {}-{})",
                    self.direction,
                    blocks.len(),
                    self.peer_ip,
                    start,
                    end,
                    actual_start,
                    actual_end
                );
            }
        } else {
            // Requested blocks are beyond our height - we don't have them yet
            debug!(
                "[{}] Cannot send blocks {}-{} to {} - we only have up to height {}",
                self.direction, start, end, self.peer_ip, our_height
            );
        }

        Ok(Some(NetworkMessage::BlocksResponse(blocks)))
    }

    /// Handle GetMasternodes request - respond with MasternodesResponse
    async fn handle_get_masternodes(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetMasternodes request from {}",
            self.direction, self.peer_ip
        );

        let all_masternodes = context.masternode_registry.list_all().await;
        let mn_data: Vec<crate::network::message::MasternodeAnnouncementData> = all_masternodes
            .iter()
            .map(|mn_info| {
                // Strip port from address to ensure consistency
                let ip_only = mn_info
                    .masternode
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&mn_info.masternode.address)
                    .to_string();
                crate::network::message::MasternodeAnnouncementData {
                    address: ip_only,
                    reward_address: mn_info.reward_address.clone(),
                    tier: mn_info.masternode.tier,
                    public_key: mn_info.masternode.public_key,
                    collateral_outpoint: mn_info.masternode.collateral_outpoint.clone(),
                    registered_at: mn_info.masternode.registered_at,
                }
            })
            .collect();

        debug!(
            "📤 [{}] Responded with {} masternode(s) to {}",
            self.direction,
            all_masternodes.len(),
            self.peer_ip
        );

        Ok(Some(NetworkMessage::MasternodesResponse(mn_data)))
    }

    /// Handle masternode inactive notification from network
    async fn handle_masternode_inactive(
        &self,
        address: String,
        timestamp: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📭 [{}] Received masternode inactive notification for {} from {}",
            self.direction, address, self.peer_ip
        );

        // Don't mark as inactive if we have a live connection to this node
        let ip_only = address.split(':').next().unwrap_or(&address);
        if context.peer_registry.is_connected(ip_only) {
            debug!(
                "⏭️ [{}] Ignoring inactive gossip for {} — we have a live connection",
                self.direction, address
            );
            return Ok(None);
        }

        match context
            .masternode_registry
            .mark_inactive_on_disconnect(&address)
            .await
        {
            Ok(()) => {
                debug!(
                    "✅ [{}] Marked masternode {} as inactive (timestamp: {})",
                    self.direction, address, timestamp
                );
            }
            Err(e) => {
                warn!(
                    "⚠️ [{}] Failed to mark masternode {} as inactive: {}",
                    self.direction, address, e
                );
            }
        }

        Ok(None)
    }

    /// Handle TimeLock Block Proposal - cache and vote
    async fn handle_timelock_block_proposal(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_height = block.header.height;

        info!(
            "📦 [{}] Received TimeLock Block proposal at height {} from {}",
            self.direction, block_height, self.peer_ip
        );

        // Validate: Only accept proposals for the next block (current + 1)
        let our_height = context.blockchain.get_height();
        let expected_height = our_height + 1;

        if block_height != expected_height {
            debug!(
                "⏭️ [{}] Rejecting block proposal at height {} (expected {})",
                self.direction, block_height, expected_height
            );
            return Ok(None);
        }

        // CRITICAL SECURITY: Validate block BEFORE voting
        // This prevents voting on blocks with invalid transactions, UTXOs, or signatures
        if let Err(e) = self.validate_block_before_vote(&block, context).await {
            warn!(
                "❌ [{}] Rejecting invalid block at height {} from {}: {}",
                self.direction, block_height, self.peer_ip, e
            );
            return Ok(None);
        }

        // VRF best-proposal selection: if we already have a proposal at this height,
        // only accept this one if it has a lower (better) VRF score
        let mut switching_vote = false;
        if let Some(cache) = &context.block_cache {
            if let Some(existing) = cache.get_by_height(block_height) {
                if existing.header.vrf_score > 0 && block.header.vrf_score > 0 {
                    if block.header.vrf_score >= existing.header.vrf_score {
                        debug!(
                            "⏭️ [{}] Rejecting block at height {} with VRF score {} (already have score {})",
                            self.direction, block_height, block.header.vrf_score, existing.header.vrf_score
                        );
                        return Ok(None);
                    }
                    info!(
                        "🎲 [{}] Better VRF score at height {}: {} < {} — switching vote",
                        self.direction,
                        block_height,
                        block.header.vrf_score,
                        existing.header.vrf_score
                    );
                    switching_vote = true;
                }
            }
        }

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // Clear stale votes from previous heights so the "first vote wins"
        // anti-double-voting rule doesn't reject votes for this new height.
        consensus.timevote.advance_vote_height(block_height);

        // Phase 3E.1: Cache the block
        let block_hash = block.hash();
        if let Some(cache) = &context.block_cache {
            cache.insert(block_hash, block.clone());
            debug!("💾 Cached block {} for voting", hex::encode(block_hash));
        }

        // Phase 3E.2: Get our node identity and look up our weight
        let validator_id = context
            .node_masternode_address
            .clone()
            .unwrap_or_else(|| format!("node_{}", self.peer_ip));
        let validator_weight = match context.masternode_registry.get(&validator_id).await {
            Some(info) => info.masternode.tier.sampling_weight().max(1),
            None => 1u64, // Default to 1 if not found
        };

        // If switching to a better VRF proposal, clear old vote first so
        // add_vote's "first vote wins" rule doesn't silently drop the new one.
        if switching_vote {
            consensus.timevote.prepare_votes.remove_voter(&validator_id);
        }

        consensus
            .timevote
            .generate_prepare_vote(block_hash, &validator_id, validator_weight);
        info!(
            "✅ [{}] Generated prepare vote for block {} at height {}",
            self.direction,
            hex::encode(block_hash),
            block.header.height
        );

        // Broadcast prepare vote to all peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            // Sign the vote with our validator key
            let sig_bytes = if let Some(signing_key) = consensus.get_signing_key() {
                use ed25519_dalek::Signer;
                let mut msg = Vec::new();
                msg.extend_from_slice(&block_hash);
                msg.extend_from_slice(validator_id.as_bytes());
                msg.extend_from_slice(b"PREPARE"); // Vote type
                signing_key.sign(&msg).to_bytes().to_vec()
            } else {
                debug!(
                    "[{}] No signing key available for prepare vote",
                    self.direction
                );
                vec![]
            };

            let prepare_vote = NetworkMessage::TimeVotePrepare {
                block_hash,
                voter_id: validator_id.clone(),
                signature: sig_bytes,
            };

            match broadcast_tx.send(prepare_vote) {
                Ok(receivers) => {
                    info!(
                        "📤 [{}] Broadcast prepare vote to {} peers",
                        self.direction, receivers
                    );
                }
                Err(_) => {
                    // Channel closed - no active receivers (peers not ready yet)
                    // This is not critical, just log at debug level
                    debug!(
                        "[{}] No active peers to broadcast prepare vote (channel closed)",
                        self.direction
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle TimeLock Prepare Vote - accumulate and check consensus
    async fn handle_timelock_prepare_vote(
        &self,
        block_hash: [u8; 32],
        voter_id: String,
        signature: Vec<u8>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "🗳️  [{}] Received prepare vote for block {} from {}",
            self.direction,
            hex::encode(block_hash),
            voter_id
        );

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // Phase 3E.2: Look up voter weight from masternode registry
        let voter_weight = Self::get_voter_weight(&context.masternode_registry, &voter_id).await;

        // Verify vote signature
        if !self
            .verify_vote_signature(
                &context.masternode_registry,
                &block_hash,
                &voter_id,
                b"PREPARE",
                &signature,
            )
            .await
            .unwrap_or(false)
        {
            return Ok(None); // Reject invalid signature
        }

        consensus
            .timevote
            .accumulate_prepare_vote(block_hash, voter_id.clone(), voter_weight);

        // Check if prepare consensus reached (>50% majority timevote)
        if consensus.timevote.check_prepare_consensus(block_hash) {
            debug!(
                "✅ [{}] Prepare consensus reached for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Generate precommit vote with actual weight
            let validator_id = context
                .node_masternode_address
                .clone()
                .unwrap_or_else(|| format!("node_{}", self.peer_ip));
            let validator_weight =
                Self::get_voter_weight(&context.masternode_registry, &validator_id)
                    .await
                    .max(1);

            consensus
                .timevote
                .generate_precommit_vote(block_hash, &validator_id, validator_weight);
            debug!(
                "✅ [{}] Generated precommit vote for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Broadcast precommit vote
            if let Some(broadcast_tx) = &context.broadcast_tx {
                // Sign the precommit vote
                let signature = if let Some(signing_key) = consensus.get_signing_key() {
                    use ed25519_dalek::Signer;
                    let mut msg = Vec::new();
                    msg.extend_from_slice(&block_hash);
                    msg.extend_from_slice(validator_id.as_bytes());
                    msg.extend_from_slice(b"PRECOMMIT"); // Vote type
                    signing_key.sign(&msg).to_bytes().to_vec()
                } else {
                    debug!(
                        "[{}] No signing key available for precommit vote",
                        self.direction
                    );
                    vec![]
                };

                let precommit_vote = NetworkMessage::TimeVotePrecommit {
                    block_hash,
                    voter_id: validator_id,
                    signature,
                };

                match broadcast_tx.send(precommit_vote) {
                    Ok(receivers) => {
                        debug!(
                            "📤 [{}] Broadcast precommit vote to {} peers",
                            self.direction, receivers
                        );
                    }
                    Err(_) => {
                        warn!(
                            "[{}] ⚠️  No active peers to broadcast precommit vote (channel closed)",
                            self.direction
                        );
                    }
                }
            }
        }

        Ok(None)
    }

    /// Handle TimeLock Precommit Vote - accumulate and finalize if consensus reached
    async fn handle_timelock_precommit_vote(
        &self,
        block_hash: [u8; 32],
        voter_id: String,
        signature: Vec<u8>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "🗳️  [{}] Received precommit vote for block {} from {}",
            self.direction,
            hex::encode(block_hash),
            voter_id
        );

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // Phase 3E.2: Look up voter weight from masternode registry
        let voter_weight = Self::get_voter_weight(&context.masternode_registry, &voter_id).await;

        // Verify vote signature
        if !self
            .verify_vote_signature(
                &context.masternode_registry,
                &block_hash,
                &voter_id,
                b"PRECOMMIT",
                &signature,
            )
            .await
            .unwrap_or(false)
        {
            return Ok(None); // Reject invalid signature
        }

        consensus
            .timevote
            .accumulate_precommit_vote(block_hash, voter_id.clone(), voter_weight);

        // Check if precommit consensus reached (>50% majority timevote)
        if consensus.timevote.check_precommit_consensus(block_hash) {
            debug!(
                "✅ [{}] Precommit consensus reached for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Phase 3E.3: Finalization Callback
            // 1. Retrieve the block from cache
            if let Some(cache) = &context.block_cache {
                if let Some(block) = cache.remove(&block_hash) {
                    // 2. Collect precommit signatures for finality proof
                    //
                    // TODO: Implement signature collection
                    //
                    // MISSING FUNCTIONALITY:
                    // The current implementation accumulates vote weights but doesn't
                    // store the actual Ed25519 signatures from precommit votes.
                    //
                    // Required changes:
                    // 1. Modify accumulate_precommit_vote() to store (voter_id, signature, weight)
                    //    instead of just aggregating weights
                    // 2. Add get_precommit_signatures(block_hash) method to retrieve them
                    // 3. Create FinalityProof with collected signatures:
                    //    ```rust
                    //    let signatures = consensus.timevote.get_precommit_signatures(block_hash)?;
                    //    let finality_proof = FinalityProof {
                    //        block_hash,
                    //        height: block.header.height,
                    //        signatures,
                    //        total_stake: precommit_weight,
                    //        timestamp: chrono::Utc::now().timestamp() as u64,
                    //    };
                    //    ```
                    //
                    // IMPACT: Without this, finality proofs lack cryptographic signatures,
                    // making them non-verifiable by light clients or external validators.
                    //
                    // PRIORITY: HIGH - Required for light client support
                    //
                    // For now, we proceed without signatures (finality still achieved via consensus)
                    let _signatures: Vec<Vec<u8>> = vec![]; // Placeholder

                    // 3. Phase 3E.3: Call timelock.finalize_block_complete()
                    // Note: This would be called through a TimeLock module instance
                    // For now, emit finalization event
                    info!(
                        "🎉 [{}] Block {} finalized with consensus!",
                        self.direction,
                        hex::encode(block_hash)
                    );
                    debug!(
                        "📦 Block height: {}, txs: {}",
                        block.header.height,
                        block.transactions.len()
                    );

                    // 4. Emit finalization event
                    // Calculate reward - constant 100 TIME per block
                    const BLOCK_REWARD_SATOSHIS: u64 = 100 * 100_000_000; // 100 TIME
                    let block_subsidy = BLOCK_REWARD_SATOSHIS;
                    let tx_fees: u64 = block.transactions.iter().map(|tx| tx.fee_amount()).sum();
                    let total_reward = block_subsidy + tx_fees;

                    debug!(
                        "💰 [{}] Block {} rewards - subsidy: {}, fees: {}, total: {:.2} TIME",
                        self.direction,
                        block.header.height,
                        block_subsidy / 100_000_000,
                        tx_fees / 100_000_000,
                        total_reward as f64 / 100_000_000.0
                    );

                    // Add block to blockchain (if not already present)
                    let current_height = context.blockchain.get_height();

                    // Skip adding genesis block if chain already has blocks
                    if block.header.height == 0 && current_height > 0 {
                        debug!(
                            "[{}] Skipping finalization add for genesis block (chain at height {})",
                            self.direction, current_height
                        );
                    } else if block.header.height > current_height {
                        let block_height = block.header.height; // Store height before move
                        info!(
                            "📥 [{}] Adding finalized block {} at height {} to blockchain (current: {})",
                            self.direction,
                            hex::encode(block_hash),
                            block_height,
                            current_height
                        );
                        match context.blockchain.add_block_with_fork_handling(block).await {
                            Ok(true) => {
                                info!(
                                    "✅ [{}] Block {} finalized via consensus!",
                                    self.direction, block_height
                                );
                            }
                            Ok(false) => {
                                debug!(
                                    "[{}] Block {} already in blockchain, skipping",
                                    self.direction, block_height
                                );
                            }
                            Err(e) => {
                                if e.contains("Block height mismatch") {
                                    let gap = block_height - current_height;
                                    warn!(
                                        "[{}] ⚠️ Block height gap detected: expected {}, got {} (gap: {})",
                                        self.direction, current_height + 1, block_height, gap
                                    );

                                    // Trigger automatic sync to fill the gap
                                    info!(
                                        "📥 Requesting missing blocks {}-{} from {}",
                                        current_height + 1,
                                        block_height - 1,
                                        self.peer_ip
                                    );

                                    let sync_msg = NetworkMessage::GetBlocks(
                                        current_height + 1,
                                        block_height - 1,
                                    );

                                    if let Err(send_err) = context
                                        .peer_registry
                                        .send_to_peer(&self.peer_ip, sync_msg)
                                        .await
                                    {
                                        warn!("Failed to request missing blocks: {}", send_err);
                                    }
                                } else {
                                    warn!(
                                        "[{}] ⚠️ Failed to add finalized block {} to blockchain: {}",
                                        self.direction, block_height, e
                                    );
                                }
                            }
                        }
                    } else {
                        debug!(
                            "[{}] Block {} already in blockchain at height {}, skipping add",
                            self.direction,
                            hex::encode(block_hash),
                            block.header.height
                        );
                    }
                    // Save precommit voters for bitmap ONLY on first finalization
                    // (cache.remove ensures this runs once — late precommits won't overwrite)
                    consensus.timevote.cleanup_block_votes(block_hash);
                } else {
                    debug!(
                        "[{}] Block {} not found in cache (likely already finalized)",
                        self.direction,
                        hex::encode(block_hash)
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle FinalityVoteBroadcast- verify signature and accumulate vote
    async fn handle_finality_vote_broadcast(
        &self,
        vote: crate::types::FinalityVote,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "[{}] Received finality vote for tx {} from {}",
            self.direction,
            hex::encode(vote.txid),
            vote.voter_mn_id
        );

        // Get voter's public key from masternode registry
        let voter_pubkey = match context.masternode_registry.get(&vote.voter_mn_id).await {
            Some(mn_info) => mn_info.masternode.public_key,
            None => {
                warn!(
                    "[{}] Received finality vote from unknown validator: {}",
                    self.direction, vote.voter_mn_id
                );
                return Ok(None);
            }
        };

        // Verify the vote signature
        if let Err(e) = vote.verify(&voter_pubkey) {
            warn!(
                "[{}] Invalid finality vote signature from {}: {}",
                self.direction, vote.voter_mn_id, e
            );
            return Ok(None);
        }

        debug!(
            "[{}] ✅ Verified finality vote signature from {}",
            self.direction, vote.voter_mn_id
        );

        // Accumulate the vote for VFP assembly
        if let Some(consensus) = &context.consensus {
            if let Err(e) = consensus.timevote.accumulate_finality_vote(vote) {
                warn!(
                    "[{}] Failed to accumulate finality vote: {}",
                    self.direction, e
                );
            }
        }

        Ok(None)
    }

    // ==================== NEW HANDLERS ====================

    /// Handle GetBlockHeight request
    async fn handle_get_block_height(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        debug!(
            "📥 [{}] Received GetBlockHeight from {}, responding with {}",
            self.direction, self.peer_ip, height
        );
        Ok(Some(NetworkMessage::BlockHeightResponse(height)))
    }

    /// Handle GetChainTip request
    async fn handle_get_chain_tip(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        let hash = match context.blockchain.get_block_hash(height) {
            Ok(h) => h,
            Err(e) => {
                // Log error but don't spam - this can happen during rapid block production
                tracing::debug!(
                    "[{}] Failed to get block hash at height {}: {} - using zero hash",
                    self.direction,
                    height,
                    e
                );
                [0u8; 32]
            }
        };
        // Only log at debug level to reduce noise
        tracing::debug!(
            "📥 [{}] GetChainTip from {}: height {} hash {}",
            self.direction,
            self.peer_ip,
            height,
            hex::encode(&hash[..8])
        );
        Ok(Some(NetworkMessage::ChainTipResponse { height, hash }))
    }

    /// Handle GetBlockRange request
    async fn handle_get_block_range(
        &self,
        start_height: u64,
        end_height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetBlockRange({}-{}) from {}",
            self.direction, start_height, end_height, self.peer_ip
        );
        let blocks = context
            .blockchain
            .get_block_range(start_height, end_height)
            .await;
        Ok(Some(NetworkMessage::BlockRangeResponse(blocks)))
    }

    /// Handle GetBlockHash request
    async fn handle_get_block_hash(
        &self,
        height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetBlockHash({}) from {}",
            self.direction, height, self.peer_ip
        );
        let hash = context.blockchain.get_block_hash_at_height(height).await;
        Ok(Some(NetworkMessage::BlockHashResponse { height, hash }))
    }

    /// Handle BlockRequest
    async fn handle_block_request(
        &self,
        height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📨 [{}] Received block request for height {} from {}",
            self.direction, height, self.peer_ip
        );

        if let Ok(block) = context.blockchain.get_block_by_height(height).await {
            debug!(
                "✅ [{}] Sending block {} to {}",
                self.direction, height, self.peer_ip
            );
            Ok(Some(NetworkMessage::BlockResponse(block)))
        } else {
            debug!(
                "⚠️ [{}] Don't have block {} requested by {}",
                self.direction, height, self.peer_ip
            );
            Ok(None)
        }
    }

    /// Handle BlockInventory - request block if we need it
    async fn handle_block_inventory(
        &self,
        block_height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_height = context.blockchain.get_height();

        if block_height > our_height {
            debug!(
                "📦 [{}] Received inventory for block {} from {}, requesting",
                self.direction, block_height, self.peer_ip
            );
            Ok(Some(NetworkMessage::BlockRequest(block_height)))
        } else {
            debug!(
                "⏭️ [{}] Ignoring inventory for block {} from {} (we're at {})",
                self.direction, block_height, self.peer_ip, our_height
            );
            Ok(None)
        }
    }

    /// Handle BlockResponse - add block to chain
    async fn handle_block_response(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_height = block.header.height;

        // Check for duplicates using dedup filter if available
        if let Some(seen_blocks) = &context.seen_blocks {
            let block_height_bytes = block_height.to_le_bytes();
            if seen_blocks.check_and_insert(&block_height_bytes).await {
                debug!(
                    "🔁 [{}] Ignoring duplicate block {} from {}",
                    self.direction, block_height, self.peer_ip
                );
                return Ok(None);
            }
        }

        info!(
            "📥 [{}] Received block {} from {}",
            self.direction, block_height, self.peer_ip
        );

        match context
            .blockchain
            .add_block_with_fork_handling(block.clone())
            .await
        {
            Ok(true) => {
                info!(
                    "✅ [{}] Added block {} from {}",
                    self.direction, block_height, self.peer_ip
                );

                // Record block for AI predictive sync and transaction analysis
                if let Some(ai) = &context.ai_system {
                    let block_time = block.header.timestamp as u64;
                    ai.predictive_sync.record_block(
                        block_height,
                        block_time,
                        600, // nominal block time
                    );
                    let tx_count = block.transactions.len();
                    if tx_count > 0 {
                        tracing::debug!(
                            "📊 Block {} contains {} transactions",
                            block_height,
                            tx_count
                        );
                    }
                }

                // Gossip inventory to other peers
                if let Some(broadcast_tx) = &context.broadcast_tx {
                    let msg = NetworkMessage::BlockInventory(block_height);
                    if let Ok(receivers) = broadcast_tx.send(msg) {
                        debug!(
                            "🔄 [{}] Gossiped block {} inventory to {} peer(s)",
                            self.direction, block_height, receivers
                        );
                    }
                }
            }
            Ok(false) => {
                let current_height = context.blockchain.get_height();
                if block_height > current_height + 1 {
                    // Block is ahead of us — immediately request missing blocks
                    let gap = block_height - current_height - 1;
                    info!(
                        "📥 [{}] Block {} is ahead of our height {} (gap: {}) — requesting missing blocks from {}",
                        self.direction, block_height, current_height, gap, self.peer_ip
                    );
                    let sync_msg = NetworkMessage::GetBlocks(current_height + 1, block_height);
                    if let Err(e) = context
                        .peer_registry
                        .send_to_peer(&self.peer_ip, sync_msg)
                        .await
                    {
                        warn!("Failed to request missing blocks: {}", e);
                    }
                } else {
                    debug!(
                        "⏭️ [{}] Skipped block {} (already have or not sequential)",
                        self.direction, block_height
                    );
                }
            }
            Err(e) => {
                if e.contains("Fork detected") || e.contains("previous_hash") {
                    // Fork detected — trigger immediate resolution
                    warn!(
                        "🔀 [{}] Fork detected with {} at block {}: {}",
                        self.direction, self.peer_ip, block_height, e
                    );
                    let current_height = context.blockchain.get_height();

                    // Request blocks going back far enough to find common ancestor
                    let request_from = current_height.saturating_sub(20).max(1);
                    info!(
                        "📥 [{}] Requesting blocks {}-{} from {} for fork resolution",
                        self.direction, request_from, block_height, self.peer_ip
                    );
                    let sync_msg = NetworkMessage::GetBlocks(request_from, block_height);
                    if let Err(send_err) = context
                        .peer_registry
                        .send_to_peer(&self.peer_ip, sync_msg)
                        .await
                    {
                        warn!("Failed to request blocks for fork resolution: {}", send_err);
                    }
                } else {
                    warn!(
                        "❌ [{}] Failed to add block {}: {}",
                        self.direction, block_height, e
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle BlockAnnouncement - legacy full block announcement
    async fn handle_block_announcement(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Same logic as BlockResponse
        self.handle_block_response(block, context).await
    }

    /// Handle GetGenesisHash - respond with our genesis block hash
    async fn handle_get_genesis_hash(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetGenesisHash from {}",
            self.direction, self.peer_ip
        );

        let genesis_hash = context.blockchain.genesis_hash();
        Ok(Some(NetworkMessage::GenesisHashResponse(genesis_hash)))
    }

    /// Handle GenesisHashResponse - verify peer's genesis matches ours
    async fn handle_genesis_hash_response(
        &self,
        peer_genesis_hash: [u8; 32],
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_genesis_hash = context.blockchain.genesis_hash();

        // If we don't have genesis yet (all zeros), we can't compare
        if our_genesis_hash == [0u8; 32] {
            debug!(
                "[{}] We don't have genesis yet, cannot verify peer {} genesis hash",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        // If peer doesn't have genesis (all zeros), skip check
        if peer_genesis_hash == [0u8; 32] {
            debug!(
                "[{}] Peer {} doesn't have genesis yet, skipping verification",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        // Compare genesis hashes
        if our_genesis_hash == peer_genesis_hash {
            info!(
                "✅ [{}] Genesis hash verified with peer {} - compatible ({})",
                self.direction,
                self.peer_ip,
                hex::encode(&our_genesis_hash[..8])
            );
            // Mark peer as genesis-compatible by resetting any fork errors
            context.peer_registry.reset_fork_errors(&self.peer_ip);
        } else {
            warn!(
                "🚫 [{}] Genesis hash MISMATCH with peer {} - INCOMPATIBLE!",
                self.direction, self.peer_ip
            );
            warn!("   Our genesis:   {}", hex::encode(&our_genesis_hash[..8]));
            warn!("   Their genesis: {}", hex::encode(&peer_genesis_hash[..8]));

            // Mark peer as permanently incompatible
            context
                .peer_registry
                .mark_genesis_incompatible(
                    &self.peer_ip,
                    &hex::encode(&our_genesis_hash[..8]),
                    &hex::encode(&peer_genesis_hash[..8]),
                )
                .await;
        }

        Ok(None)
    }

    /// Handle RequestGenesis
    async fn handle_request_genesis(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received genesis request from {}",
            self.direction, self.peer_ip
        );

        match context.blockchain.get_block_by_height(0).await {
            Ok(genesis) => {
                info!(
                    "📤 [{}] Sending genesis block to {}",
                    self.direction, self.peer_ip
                );
                Ok(Some(NetworkMessage::GenesisAnnouncement(genesis)))
            }
            Err(_) => {
                debug!(
                    "⚠️ [{}] Cannot fulfill genesis request - we don't have genesis yet",
                    self.direction
                );
                Ok(None)
            }
        }
    }

    /// Handle GenesisAnnouncement
    async fn handle_genesis_announcement(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Verify this is actually a genesis block
        if block.header.height != 0 {
            warn!(
                "⚠️ [{}] Received GenesisAnnouncement for non-genesis block {} from {}",
                self.direction, block.header.height, self.peer_ip
            );
            return Ok(None);
        }

        // Check if we already have genesis
        if context.blockchain.get_block_by_height(0).await.is_ok() {
            debug!(
                "⏭️ [{}] Ignoring genesis announcement (already have genesis) from {}",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        info!(
            "📦 [{}] Received genesis announcement from {}",
            self.direction, self.peer_ip
        );

        // Verify basic genesis structure
        use crate::block::genesis::GenesisBlock;
        match GenesisBlock::verify_structure(&block) {
            Ok(()) => {
                info!(
                    "✅ [{}] Genesis structure validation passed, adding to chain",
                    self.direction
                );

                match context.blockchain.add_block(block.clone()).await {
                    Ok(()) => {
                        info!("✅ [{}] Genesis block added successfully", self.direction);

                        // Broadcast to other peers
                        if let Some(broadcast_tx) = &context.broadcast_tx {
                            let msg = NetworkMessage::GenesisAnnouncement(block);
                            let _ = broadcast_tx.send(msg);
                        }
                    }
                    Err(e) => {
                        warn!("❌ [{}] Failed to add genesis block: {}", self.direction, e);
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ [{}] Genesis validation failed: {}", self.direction, e);
            }
        }

        Ok(None)
    }

    /// Handle TransactionBroadcast
    async fn handle_transaction_broadcast(
        &self,
        tx: crate::types::Transaction,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let txid = tx.txid();

        // Check for duplicates
        if let Some(seen_transactions) = &context.seen_transactions {
            if seen_transactions.check_and_insert(&txid).await {
                debug!(
                    "🔁 [{}] Ignoring duplicate transaction {} from {}",
                    self.direction,
                    hex::encode(&txid[..8]),
                    self.peer_ip
                );
                return Ok(None);
            }
        }

        debug!(
            "📥 [{}] Received transaction {} from {}",
            self.direction,
            hex::encode(&txid[..8]),
            self.peer_ip
        );

        // Record transaction for AI attack detection (double-spend tracking)
        if let Some(ai) = &context.ai_system {
            ai.attack_detector
                .record_transaction(&hex::encode(&txid[..8]), &self.peer_ip);
        }

        // Process transaction through consensus
        if let Some(consensus) = &context.consensus {
            match consensus.process_transaction(tx.clone()).await {
                Ok(_) => {
                    debug!(
                        "✅ [{}] Transaction {} processed",
                        self.direction,
                        hex::encode(&txid[..8])
                    );

                    // Gossip to other peers
                    if let Some(broadcast_tx) = &context.broadcast_tx {
                        let msg = NetworkMessage::TransactionBroadcast(tx);
                        if let Ok(receivers) = broadcast_tx.send(msg) {
                            debug!(
                                "🔄 [{}] Gossiped transaction to {} peer(s)",
                                self.direction, receivers
                            );
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "⚠️ [{}] Transaction {} rejected: {}",
                        self.direction,
                        hex::encode(&txid[..8]),
                        e
                    );
                }
            }
        } else {
            debug!(
                "⚠️ [{}] No consensus engine to process transaction",
                self.direction
            );
        }

        Ok(None)
    }

    /// Handle GetPeers request
    async fn handle_get_peers(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetPeers request from {}",
            self.direction, self.peer_ip
        );

        // Use peer_manager if available, otherwise use peer_registry
        let peers = if let Some(peer_manager) = &context.peer_manager {
            peer_manager.get_all_peers().await
        } else {
            context.peer_registry.get_connected_peers().await
        };

        debug!(
            "📤 [{}] Sending {} peer(s) to {}",
            self.direction,
            peers.len(),
            self.peer_ip
        );
        Ok(Some(NetworkMessage::PeersResponse(peers)))
    }

    /// Handle PeersResponse
    async fn handle_peers_response(
        &self,
        peers: Vec<String>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received PeersResponse from {} with {} peer(s)",
            self.direction,
            self.peer_ip,
            peers.len()
        );

        // Add to peer_manager if available
        if let Some(peer_manager) = &context.peer_manager {
            let mut added = 0;
            for peer_addr in &peers {
                if peer_manager.add_peer_candidate(peer_addr.clone()).await {
                    added += 1;
                }
            }
            if added > 0 {
                info!(
                    "✓ [{}] Added {} new peer candidate(s) from {}",
                    self.direction, added, self.peer_ip
                );
            }
        } else {
            // Fallback to peer_registry discovered_peers
            context.peer_registry.add_discovered_peers(&peers).await;
        }

        Ok(None)
    }

    /// Handle MasternodeAnnouncement
    async fn handle_masternode_announcement(
        &self,
        _address: String,
        reward_address: String,
        tier: crate::types::MasternodeTier,
        public_key: ed25519_dalek::VerifyingKey,
        collateral_outpoint: Option<crate::types::OutPoint>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let peer_ip = self.peer_ip.clone();

        debug!(
            "📨 [{}] Received masternode announcement from {} (tier: {:?})",
            self.direction, peer_ip, tier
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if tier != crate::types::MasternodeTier::Free {
            // Staked tiers MUST include collateral_outpoint
            let outpoint = match collateral_outpoint {
                Some(op) => op,
                None => {
                    warn!(
                        "❌ [{}] Rejecting {:?} masternode from {} — no collateral outpoint",
                        self.direction, tier, peer_ip
                    );
                    return Ok(None);
                }
            };

            // Verify collateral UTXO on-chain
            if let Some(utxo_manager) = &context.utxo_manager {
                match utxo_manager.get_utxo(&outpoint).await {
                    Ok(utxo) => {
                        let required = tier.collateral();
                        if utxo.value != required {
                            warn!(
                                "❌ [{}] Rejecting {:?} masternode from {} — collateral {} != required {}",
                                self.direction, tier, peer_ip, utxo.value, required
                            );
                            return Ok(None);
                        }
                        if utxo_manager.is_collateral_locked(&outpoint) {
                            let existing = utxo_manager.get_locked_collateral(&outpoint);
                            if existing
                                .map(|info| info.masternode_address != peer_ip)
                                .unwrap_or(false)
                            {
                                warn!(
                                    "❌ [{}] Rejecting masternode from {} — collateral already locked by another",
                                    self.direction, peer_ip
                                );
                                return Ok(None);
                            }
                        }
                        debug!(
                            "✅ [{}] Collateral verified for {:?} masternode {} ({} TIME)",
                            self.direction,
                            tier,
                            peer_ip,
                            utxo.value as f64 / 100_000_000.0
                        );
                    }
                    Err(_) => {
                        warn!(
                            "❌ [{}] Rejecting {:?} masternode from {} — collateral UTXO not found on-chain",
                            self.direction, tier, peer_ip
                        );
                        return Ok(None);
                    }
                }

                // Lock the collateral
                let lock_height = context.blockchain.get_height();
                let _ = utxo_manager.lock_collateral(
                    outpoint.clone(),
                    peer_ip.clone(),
                    lock_height,
                    tier.collateral(),
                );
            } else {
                warn!(
                    "⚠️ [{}] Cannot verify collateral for {} — no UTXO manager available",
                    self.direction, peer_ip
                );
                return Ok(None);
            }

            // Create masternode with verified collateral
            let mn = crate::types::Masternode::new_with_collateral(
                peer_ip.clone(),
                reward_address.clone(),
                tier.collateral(),
                outpoint,
                public_key,
                tier,
                now,
            );

            match context
                .masternode_registry
                .register(mn, reward_address)
                .await
            {
                Ok(()) => {
                    let count = context.masternode_registry.total_count().await;
                    debug!(
                        "✅ [{}] Registered {:?} masternode {} (total: {})",
                        self.direction, tier, peer_ip, count
                    );
                    if let Some(peer_manager) = &context.peer_manager {
                        peer_manager.add_peer(peer_ip).await;
                    }
                }
                Err(e) => {
                    warn!(
                        "❌ [{}] Failed to register masternode {}: {}",
                        self.direction, peer_ip, e
                    );
                }
            }
        } else {
            // Free tier — no collateral verification needed
            let mn = crate::types::Masternode::new_legacy(
                peer_ip.clone(),
                reward_address.clone(),
                0,
                public_key,
                tier,
                now,
            );

            match context
                .masternode_registry
                .register(mn, reward_address)
                .await
            {
                Ok(()) => {
                    let count = context.masternode_registry.total_count().await;
                    debug!(
                        "✅ [{}] Registered Free masternode {} (total: {})",
                        self.direction, peer_ip, count
                    );
                    if let Some(peer_manager) = &context.peer_manager {
                        peer_manager.add_peer(peer_ip).await;
                    }
                }
                Err(e) => {
                    warn!(
                        "❌ [{}] Failed to register masternode {}: {}",
                        self.direction, peer_ip, e
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle MasternodeUnlock announcement (deprecated — masternode management is now config-based)
    async fn handle_masternode_unlock(
        &self,
        address: String,
        _collateral_outpoint: crate::types::OutPoint,
        _timestamp: u64,
        _context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        warn!(
            "⚠️ [{}] Ignoring MasternodeUnlock from {} for {} (deprecated — use config.toml)",
            self.direction, self.peer_ip, address
        );
        Ok(None)
    }

    /// Handle MasternodesResponse
    async fn handle_masternodes_response(
        &self,
        masternodes: Vec<crate::network::message::MasternodeAnnouncementData>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received MasternodesResponse from {} with {} masternode(s)",
            self.direction,
            self.peer_ip,
            masternodes.len()
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // BOOTSTRAP MODE: At genesis (height 0), mark masternodes as active
        // This allows fresh nodes to discover each other and produce first blocks
        let current_height = context.blockchain.get_height();
        let is_bootstrap = current_height == 0;

        // Get local masternode address to skip self-overwrites from peer gossip
        let local_address = context.masternode_registry.get_local_address().await;

        let mut registered = 0;
        for mn_data in masternodes {
            // Don't let peer gossip overwrite our own masternode entry
            if let Some(ref local_addr) = local_address {
                let mn_ip = mn_data
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&mn_data.address);
                let local_ip = local_addr.split(':').next().unwrap_or(local_addr);
                if mn_ip == local_ip {
                    continue;
                }
            }

            let masternode = crate::types::Masternode::new_legacy(
                mn_data.address.clone(),
                mn_data.reward_address.clone(),
                0,
                mn_data.public_key,
                mn_data.tier,
                now,
            );

            // BOOTSTRAP: Mark as active at genesis to allow block production
            // NORMAL: Register as inactive (will become active via direct P2P connection)
            let should_activate = is_bootstrap;

            if context
                .masternode_registry
                .register_internal(masternode, mn_data.reward_address, should_activate)
                .await
                .is_ok()
            {
                registered += 1;
            }
        }

        if registered > 0 {
            if is_bootstrap {
                info!(
                    "✓ [{}] Bootstrap mode: Registered {} masternode(s) as ACTIVE from peer exchange",
                    self.direction, registered
                );
            } else {
                debug!(
                    "✓ [{}] Registered {} masternode(s) from peer exchange",
                    self.direction, registered
                );
            }
        }

        Ok(None)
    }

    /// Handle GetLockedCollaterals request
    async fn handle_get_locked_collaterals(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received GetLockedCollaterals request from {}",
            self.direction, self.peer_ip
        );

        // Get all locked collaterals from UTXO manager
        if let Some(utxo_manager) = &context.utxo_manager {
            let locked_collaterals = utxo_manager.list_locked_collaterals();

            let collateral_data: Vec<crate::network::message::LockedCollateralData> =
                locked_collaterals
                    .into_iter()
                    .map(|lc| crate::network::message::LockedCollateralData {
                        outpoint: lc.outpoint,
                        masternode_address: lc.masternode_address,
                        lock_height: lc.lock_height,
                        locked_at: lc.locked_at,
                        amount: lc.amount,
                    })
                    .collect();

            info!(
                "📤 [{}] Responded with {} locked collateral(s) to {}",
                self.direction,
                collateral_data.len(),
                self.peer_ip
            );

            Ok(Some(NetworkMessage::LockedCollateralsResponse(
                collateral_data,
            )))
        } else {
            // No UTXO manager available, return empty list
            Ok(Some(NetworkMessage::LockedCollateralsResponse(Vec::new())))
        }
    }

    /// Handle LockedCollateralsResponse
    async fn handle_locked_collaterals_response(
        &self,
        collaterals: Vec<crate::network::message::LockedCollateralData>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received LockedCollateralsResponse from {} with {} collateral(s)",
            self.direction,
            self.peer_ip,
            collaterals.len()
        );

        if let Some(utxo_manager) = &context.utxo_manager {
            let mut synced = 0;
            let mut conflicts = 0;
            let mut invalid = 0;

            for collateral_data in collaterals {
                // Verify the UTXO exists in our UTXO set
                match utxo_manager.get_utxo(&collateral_data.outpoint).await {
                    Ok(utxo) => {
                        // Verify amount matches
                        if utxo.value != collateral_data.amount {
                            warn!(
                                "⚠️ [{}] Collateral amount mismatch for {:?}: expected {}, got {}",
                                self.direction,
                                collateral_data.outpoint,
                                collateral_data.amount,
                                utxo.value
                            );
                            invalid += 1;
                            continue;
                        }

                        // Check if already locked
                        if utxo_manager.is_collateral_locked(&collateral_data.outpoint) {
                            // Already locked - potential conflict or duplicate
                            let existing =
                                utxo_manager.get_locked_collateral(&collateral_data.outpoint);

                            if let Some(existing_lock) = existing {
                                if existing_lock.masternode_address
                                    != collateral_data.masternode_address
                                {
                                    warn!(
                                        "⚠️ [{}] Collateral conflict for {:?}: locked by {} (peer says {})",
                                        self.direction,
                                        collateral_data.outpoint,
                                        existing_lock.masternode_address,
                                        collateral_data.masternode_address
                                    );
                                    conflicts += 1;
                                }
                                // else: same lock, no action needed
                            }
                            continue;
                        }

                        // Lock the collateral
                        match utxo_manager.lock_collateral(
                            collateral_data.outpoint.clone(),
                            collateral_data.masternode_address.clone(),
                            collateral_data.lock_height,
                            collateral_data.amount,
                        ) {
                            Ok(()) => {
                                synced += 1;
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️ [{}] Failed to lock collateral {:?}: {:?}",
                                    self.direction, collateral_data.outpoint, e
                                );
                                invalid += 1;
                            }
                        }
                    }
                    Err(_) => {
                        // UTXO doesn't exist in our set
                        warn!(
                            "⚠️ [{}] Collateral UTXO {:?} not found in our UTXO set",
                            self.direction, collateral_data.outpoint
                        );
                        invalid += 1;
                    }
                }
            }

            if synced > 0 {
                info!(
                    "✓ [{}] Synced {} locked collateral(s) from peer (conflicts: {}, invalid: {})",
                    self.direction, synced, conflicts, invalid
                );
            }
        }

        Ok(None)
    }

    /// Handle UTXOStateQuery
    async fn handle_utxo_state_query(
        &self,
        outpoints: Vec<crate::types::OutPoint>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received UTXOStateQuery for {} outpoints from {}",
            self.direction,
            outpoints.len(),
            self.peer_ip
        );

        if let Some(utxo_manager) = &context.utxo_manager {
            let mut responses = Vec::new();
            for op in &outpoints {
                if let Some(state) = utxo_manager.get_state(op) {
                    responses.push((op.clone(), state));
                }
            }
            Ok(Some(NetworkMessage::UTXOStateResponse(responses)))
        } else {
            debug!(
                "⚠️ [{}] No UTXO manager to handle state query",
                self.direction
            );
            Ok(None)
        }
    }

    /// Handle GetUTXOStateHash
    async fn handle_get_utxo_state_hash(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        let utxo_hash = context.blockchain.get_utxo_state_hash().await;
        let utxo_count = context.blockchain.get_utxo_count().await;

        debug!(
            "📤 [{}] Sending UTXO state hash to {}",
            self.direction, self.peer_ip
        );
        Ok(Some(NetworkMessage::UTXOStateHashResponse {
            hash: utxo_hash,
            height,
            utxo_count,
        }))
    }

    /// Handle GetUTXOSet
    async fn handle_get_utxo_set(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let utxos = context.blockchain.get_all_utxos().await;
        info!(
            "📤 [{}] Sending complete UTXO set ({} utxos) to {}",
            self.direction,
            utxos.len(),
            self.peer_ip
        );
        Ok(Some(NetworkMessage::UTXOSetResponse(utxos)))
    }

    /// Handle ConsensusQuery
    async fn handle_consensus_query(
        &self,
        height: u64,
        block_hash: [u8; 32],
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received ConsensusQuery for height {} from {}",
            self.direction, height, self.peer_ip
        );

        let (agrees, our_hash) = context
            .blockchain
            .check_consensus_with_peer(height, block_hash)
            .await;
        Ok(Some(NetworkMessage::ConsensusQueryResponse {
            agrees,
            height,
            their_hash: our_hash.unwrap_or([0u8; 32]),
        }))
    }

    /// Handle GetChainWork
    async fn handle_get_chain_work(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        let tip_hash = context
            .blockchain
            .get_block_hash_at_height(height)
            .await
            .unwrap_or([0u8; 32]);
        let cumulative_work = context.blockchain.get_cumulative_work().await;

        debug!(
            "📤 [{}] Sending chain work response to {}: height={}, work={}",
            self.direction, self.peer_ip, height, cumulative_work
        );
        Ok(Some(NetworkMessage::ChainWorkResponse {
            height,
            tip_hash,
            cumulative_work,
        }))
    }

    /// Handle GetChainWorkAt
    async fn handle_get_chain_work_at(
        &self,
        height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_hash = context
            .blockchain
            .get_block_hash_at_height(height)
            .await
            .unwrap_or([0u8; 32]);
        let cumulative_work = context
            .blockchain
            .get_work_at_height(height)
            .await
            .unwrap_or(0);

        debug!(
            "📤 [{}] Sending chain work at height {} to {}",
            self.direction, height, self.peer_ip
        );
        Ok(Some(NetworkMessage::ChainWorkAtResponse {
            height,
            block_hash,
            cumulative_work,
        }))
    }

    /// Handle ForkAlert
    async fn handle_fork_alert(
        &self,
        your_height: u64,
        your_hash: [u8; 32],
        consensus_height: u64,
        consensus_hash: [u8; 32],
        consensus_peer_count: usize,
        message: String,
    ) -> Result<Option<NetworkMessage>, String> {
        warn!(
            "🚨 [{}] FORK ALERT from {}: {}",
            self.direction, self.peer_ip, message
        );
        warn!(
            "   Our height {} hash {} vs Consensus height {} hash {} ({} peers)",
            your_height,
            hex::encode(&your_hash[..8]),
            consensus_height,
            hex::encode(&consensus_hash[..8]),
            consensus_peer_count
        );

        // If we're on the minority fork, request consensus chain.
        // Check both same-height forks and height mismatches (we may have
        // advanced further on our fork, or fallen behind).
        let our_hash_differs = your_hash != consensus_hash;
        let heights_close = your_height.abs_diff(consensus_height) <= 5;

        if our_hash_differs && heights_close {
            warn!(
                "   ⚠️ We appear to be on minority fork (our height {} vs consensus {})! Requesting consensus chain...",
                your_height, consensus_height
            );
            let request_from = consensus_height.saturating_sub(10);
            let request_to = your_height.max(consensus_height) + 5;
            return Ok(Some(NetworkMessage::GetBlocks(request_from, request_to)));
        }

        Ok(None)
    }

    /// Handle ChainTipResponse - centralized fork detection and sync triggering
    ///
    /// This replaces the duplicated logic that was in peer_connection.rs
    async fn handle_chain_tip_response(
        &self,
        peer_height: u64,
        peer_hash: [u8; 32],
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_height = context.blockchain.get_height();
        let our_hash = context
            .blockchain
            .get_block_hash(our_height)
            .unwrap_or([0u8; 32]);

        // Update peer registry with their height and chain tip
        context
            .peer_registry
            .set_peer_height(&self.peer_ip, peer_height)
            .await;
        context
            .peer_registry
            .update_peer_chain_tip(&self.peer_ip, peer_height, peer_hash)
            .await;

        tracing::debug!(
            "[{}] ChainTipResponse from {}: height {} hash {} (our height: {})",
            self.direction,
            self.peer_ip,
            peer_height,
            hex::encode(&peer_hash[..8]),
            our_height
        );

        if peer_height == our_height {
            // Same height - check if same hash (on same chain)
            if peer_hash != our_hash {
                // FORK DETECTED - same height but different blocks!
                // Rate-limit: only log once per 60s per peer to avoid flooding
                let now = Instant::now();
                let should_log = {
                    let last = self.last_fork_warning.lock().unwrap();
                    match *last {
                        Some(t) => now.duration_since(t) >= Duration::from_secs(60),
                        None => true,
                    }
                };
                if should_log {
                    *self.last_fork_warning.lock().unwrap() = Some(now);
                    warn!(
                        "🔀 [{}] FORK with {} at height {}: our {} vs their {}",
                        self.direction,
                        self.peer_ip,
                        peer_height,
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    );
                }

                // Check consensus - if we have majority, alert the peer
                // CRITICAL: Only count compatible peers (same genesis) for fork consensus
                let all_peers = context.peer_registry.get_compatible_peers().await;
                let mut our_chain_count = 1; // Count ourselves
                let mut peer_chain_count = 0;

                for peer_addr in &all_peers {
                    if let Some((peer_h, p_hash)) =
                        context.peer_registry.get_peer_chain_tip(peer_addr).await
                    {
                        if peer_h == our_height {
                            if p_hash == our_hash {
                                our_chain_count += 1;
                            } else if p_hash == peer_hash {
                                peer_chain_count += 1;
                            }
                        }
                    }
                }

                // If we have consensus and peer is on minority fork, send alert
                if our_chain_count > peer_chain_count && our_chain_count >= 3 {
                    info!(
                        "📢 [{}] We have consensus ({} vs {} peers) at height {} - sending fork alert to {}",
                        self.direction, our_chain_count, peer_chain_count, peer_height, self.peer_ip
                    );

                    // Return ForkAlert message to be sent
                    return Ok(Some(NetworkMessage::ForkAlert {
                        your_height: peer_height,
                        your_hash: peer_hash,
                        consensus_height: our_height,
                        consensus_hash: our_hash,
                        consensus_peer_count: our_chain_count,
                        message: format!(
                            "You're on a minority fork at height {}. {} peers (including us) are on consensus chain with hash {}",
                            peer_height,
                            our_chain_count,
                            hex::encode(&our_hash[..8])
                        ),
                    }));
                }

                // Request blocks for fork resolution
                let request_from = peer_height.saturating_sub(10);
                info!(
                    "🔄 [{}] Requesting blocks {}-{} from {} for fork resolution",
                    self.direction,
                    request_from,
                    peer_height + 5,
                    self.peer_ip
                );
                return Ok(Some(NetworkMessage::GetBlocks(
                    request_from,
                    peer_height + 5,
                )));
            } else {
                debug!(
                    "✅ [{}] Peer {} on same chain at height {}",
                    self.direction, self.peer_ip, peer_height
                );
            }
        } else if peer_height > our_height {
            // Peer is ahead - check if they're part of consensus before syncing
            let is_consensus_peer = context.blockchain.is_peer_in_consensus(&self.peer_ip).await;

            if !is_consensus_peer {
                warn!(
                    "🚫 [{}] Ignoring blocks from non-consensus peer {} at height {} (we have {})",
                    self.direction, self.peer_ip, peer_height, our_height
                );
                return Ok(None);
            }

            // Peer is ahead and in consensus - sync from them
            debug!(
                "📈 [{}] Peer {} ahead at height {} (we have {}), requesting blocks",
                self.direction, self.peer_ip, peer_height, our_height
            );
            return Ok(Some(NetworkMessage::GetBlocks(
                our_height + 1,
                peer_height + 1,
            )));
        } else {
            // We're ahead - peer might need to sync from us
            debug!(
                "📉 [{}] Peer {} behind at height {} (we have {})",
                self.direction, self.peer_ip, peer_height, our_height
            );
        }

        Ok(None)
    }

    /// Handle BlocksResponse/BlockRangeResponse - centralized block processing
    ///
    /// This replaces the duplicated logic that was in peer_connection.rs
    async fn handle_blocks_response(
        &self,
        blocks: Vec<Block>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_count = blocks.len();
        if block_count == 0 {
            debug!(
                "📥 [{}] Received empty blocks response from {}",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        let start_height = blocks.first().map(|b| b.header.height).unwrap_or(0);
        let end_height = blocks.last().map(|b| b.header.height).unwrap_or(0);

        // Check if peer is whitelisted
        let is_whitelisted = context.peer_registry.is_whitelisted(&self.peer_ip).await;

        info!(
            "📥 [{}] Received {} blocks (height {}-{}) from {} {}",
            self.direction,
            block_count,
            start_height,
            end_height,
            self.peer_ip,
            if is_whitelisted { "(whitelisted)" } else { "" }
        );

        // Check if we're in fork resolution state - if so, route blocks to fork handler
        {
            use crate::blockchain::ForkResolutionState;
            let fork_state = context.blockchain.fork_state.read().await;
            if let ForkResolutionState::FetchingChain { peer_addr, .. } = &*fork_state {
                if peer_addr == &self.peer_ip {
                    info!(
                        "📥 [{}] Received blocks from {} match active fork resolution - routing to handle_fork()",
                        self.direction, self.peer_ip
                    );
                    drop(fork_state); // Release lock before async call

                    let peer_ip = self.peer_ip.clone();
                    let blockchain = context.blockchain.clone();

                    // Pass blocks to fork handler
                    tokio::spawn(async move {
                        if let Err(e) = blockchain.handle_fork(blocks, peer_ip).await {
                            warn!("Fork resolution with new blocks failed: {}", e);
                        }
                    });

                    return Ok(None);
                }
            }
        }

        // Try to add blocks sequentially
        let mut added = 0;
        let mut skipped = 0;
        let mut fork_detected = false;

        for block in blocks.iter() {
            // Validate block has non-zero previous_hash (except genesis at height 0)
            if block.header.height > 0 && block.header.previous_hash == [0u8; 32] {
                warn!(
                    "⚠️ [{}] Peer {} sent corrupt block {} with zero previous_hash - skipping",
                    self.direction, self.peer_ip, block.header.height
                );
                skipped += 1;
                if is_whitelisted {
                    warn!(
                        "⚠️ [{}] Whitelisted peer {} sent corrupt block - data quality issue!",
                        self.direction, self.peer_ip
                    );
                }
                continue;
            }

            match context
                .blockchain
                .add_block_with_fork_handling(block.clone())
                .await
            {
                Ok(true) => {
                    added += 1;

                    // Reset persistent fork error counter on successful block
                    context.peer_registry.reset_fork_errors(&self.peer_ip);

                    // Clear incompatible status if blocks now work
                    if added == 1 {
                        context
                            .peer_registry
                            .clear_incompatible(&self.peer_ip)
                            .await;
                    }
                }
                Ok(false) => {
                    // Block already exists or is not next in chain
                    debug!(
                        "⏭️ [{}] Skipped block {} from {} (already exists or not sequential)",
                        self.direction, block.header.height, self.peer_ip
                    );
                    skipped += 1;
                }
                Err(e) if e.contains("Fork detected") || e.contains("previous_hash") => {
                    fork_detected = true;
                    skipped += 1;

                    debug!(
                        "🔀 [{}] Fork detected from {}: {}",
                        self.direction, self.peer_ip, e
                    );

                    // Track fork errors (for metrics/debugging)
                    let _error_count = context.peer_registry.increment_fork_errors(&self.peer_ip);

                    // IMMEDIATE fork resolution - don't wait for multiple errors
                    // If we detect a fork, we need to resolve it right away
                    warn!(
                        "🔀 [{}] Fork detected with peer {} at height {}: {}",
                        self.direction, self.peer_ip, block.header.height, e
                    );

                    // Trigger immediate fork resolution check
                    info!(
                        "🔄 [{}] Fork with {} - initiating immediate resolution",
                        self.direction, self.peer_ip
                    );

                    // Collect all fork blocks for resolution
                    let fork_blocks = blocks.to_vec();
                    let peer_ip = self.peer_ip.clone();
                    let blockchain = context.blockchain.clone();

                    // Trigger fork resolution in background
                    tokio::spawn(async move {
                        if let Err(e) = blockchain.handle_fork(fork_blocks, peer_ip).await {
                            warn!("Fork resolution failed: {}", e);
                        }
                    });

                    // Stop processing remaining blocks - let fork resolution handle it
                    break;
                }
                Err(e) if e.contains("corrupted") || e.contains("serialization failed") => {
                    // SECURITY: Corrupted block is a SEVERE violation - potential attack
                    error!(
                        "🚨 [{}] CORRUPTED BLOCK {} from {} - potential attack: {}",
                        self.direction, block.header.height, self.peer_ip, e
                    );

                    // Record severe violation and potentially ban the peer
                    if self.peer_ip.parse::<std::net::IpAddr>().is_ok() {
                        // Mark peer as incompatible - they have corrupted data
                        // Corrupted blocks are temporary (might be software bug, not permanent)
                        context
                            .peer_registry
                            .mark_incompatible(
                                &self.peer_ip,
                                &format!("Sent corrupted block {}: {}", block.header.height, e),
                                false, // temporary - will be rechecked
                            )
                            .await;
                    }

                    // Stop processing ALL blocks from this peer in this batch
                    warn!(
                        "🚫 [{}] Rejecting all {} blocks from {} due to corruption",
                        self.direction, block_count, self.peer_ip
                    );
                    return Err(format!(
                        "Peer {} sent corrupted block - connection should be terminated",
                        self.peer_ip
                    ));
                }
                Err(e) => {
                    warn!(
                        "❌ [{}] Failed to add block {} from {}: {}",
                        self.direction, block.header.height, self.peer_ip, e
                    );
                    skipped += 1;
                }
            }
        }

        if added > 0 {
            info!(
                "✅ [{}] Added {} blocks from {} (skipped {})",
                self.direction, added, self.peer_ip, skipped
            );
        } else if skipped > 0 && !fork_detected {
            // No blocks added — check if we're behind and need to request missing blocks
            let current_height = context.blockchain.get_height();
            if start_height > current_height + 1 {
                info!(
                    "📥 [{}] Blocks {}-{} are ahead of our height {} — requesting missing blocks from {}",
                    self.direction, start_height, end_height, current_height, self.peer_ip
                );
                let sync_msg = NetworkMessage::GetBlocks(current_height + 1, end_height);
                if let Err(e) = context
                    .peer_registry
                    .send_to_peer(&self.peer_ip, sync_msg)
                    .await
                {
                    warn!("Failed to request missing blocks: {}", e);
                }
            } else {
                warn!(
                    "⚠️ [{}] No blocks added from {} - all {} blocks skipped (likely not sequential with our chain at height {})",
                    self.direction,
                    self.peer_ip,
                    skipped,
                    current_height
                );
            }
        }

        if fork_detected {
            warn!(
                "⚠️ [{}] All {} blocks skipped from {} (fork detected)",
                self.direction, block_count, self.peer_ip
            );
        }

        Ok(None)
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - Message Handlers
    // ========================================================================

    /// Handle LivenessAlert message (§7.6.2)
    /// Node receives alert that a transaction has stalled
    async fn handle_liveness_alert(
        &self,
        alert: crate::types::LivenessAlert,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let txid_hex = hex::encode(alert.txid);

        info!(
            "[{}] Received LivenessAlert for tx {} from {} (stall: {}ms, confidence: {})",
            self.direction,
            txid_hex,
            alert.reporter_mn_id,
            alert.stall_duration_ms,
            alert.current_confidence
        );

        // Verify the alert signature - find masternode by address
        let masternodes = context.masternode_registry.list_all().await;
        let masternode = masternodes
            .iter()
            .find(|mn| mn.masternode.address == alert.reporter_mn_id)
            .ok_or_else(|| {
                format!(
                    "Reporter {} not in masternode registry",
                    alert.reporter_mn_id
                )
            })?;

        alert
            .verify(&masternode.masternode.public_key)
            .map_err(|e| format!("Invalid LivenessAlert signature: {}", e))?;

        // Forward to consensus engine if we have one
        if let Some(consensus) = &context.consensus {
            // Phase 4: Detect equivocation before processing
            if consensus.detect_alert_equivocation(&alert.txid, &alert.reporter_mn_id) {
                consensus.flag_byzantine(&alert.reporter_mn_id, "Alert equivocation detected");
                return Err(format!(
                    "Rejecting alert from {}: equivocation detected",
                    alert.reporter_mn_id
                ));
            }

            // Check if we also observe this stall
            if let Some(tx_status) = consensus.get_tx_status(&alert.txid) {
                if matches!(tx_status, crate::types::TransactionStatus::Voting { .. }) {
                    // We also see this transaction in Voting state
                    let stalled = consensus.check_stall_timeout(&alert.txid);

                    if stalled {
                        info!("[{}] Confirming stall for tx {}", self.direction, txid_hex);

                        // §7.6 Week 5-6: Accumulate alerts and check f+1 threshold
                        let should_trigger_fallback =
                            consensus.accumulate_liveness_alert(alert.clone(), masternodes.len());

                        let alert_count = consensus.get_alert_count(&alert.txid);
                        let n = masternodes.len();
                        let f = (n.saturating_sub(1)) / 3;
                        let threshold = f + 1;

                        info!(
                            "[{}] Alert accumulation for tx {}: {}/{} (threshold: {})",
                            self.direction, txid_hex, alert_count, n, threshold
                        );

                        // Trigger fallback if f+1 threshold reached
                        if should_trigger_fallback {
                            warn!(
                                "[{}] 🚨 Fallback triggered for tx {} ({} >= {} alerts)",
                                self.direction, txid_hex, alert_count, threshold
                            );

                            // Transition to FallbackResolution state
                            consensus
                                .transition_to_fallback_resolution(alert.txid, alert_count as u32);
                        }
                    }
                }
            }
        }

        // Relay the alert to other peers (gossip protocol)
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let _ = broadcast_tx.send(NetworkMessage::LivenessAlert { alert });
        }

        Ok(None)
    }

    /// Handle FinalityProposal message (§7.6.4 Step 3)
    /// Deterministic leader proposes Accept/Reject decision
    async fn handle_finality_proposal(
        &self,
        proposal: crate::types::FinalityProposal,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let txid_hex = hex::encode(proposal.txid);

        info!(
            "[{}] Received FinalityProposal for tx {} from leader {} (decision: {:?})",
            self.direction, txid_hex, proposal.leader_mn_id, proposal.decision
        );

        // Verify the proposal signature - find masternode by address
        let masternodes = context.masternode_registry.list_all().await;
        let leader = masternodes
            .iter()
            .find(|mn| mn.masternode.address == proposal.leader_mn_id)
            .ok_or_else(|| {
                format!(
                    "Leader {} not in masternode registry",
                    proposal.leader_mn_id
                )
            })?;

        proposal
            .verify(&leader.masternode.public_key)
            .map_err(|e| format!("Invalid FinalityProposal signature: {}", e))?;

        // §7.6 Week 5-6 Part 2: Register proposal and prepare for voting
        if let Some(consensus) = &context.consensus {
            // Register the mapping so we can finalize when votes come in
            let proposal_hash = proposal.proposal_hash();
            consensus.register_proposal(proposal_hash, proposal.txid);

            info!(
                "[{}] Registered proposal {} for tx {}",
                self.direction,
                hex::encode(proposal_hash),
                txid_hex
            );

            // Phase 4: Detect Byzantine behavior (multiple proposals for same tx)
            let proposals_for_tx = consensus.detect_multiple_proposals(&proposal.txid);
            if proposals_for_tx.len() > 1 {
                consensus.flag_byzantine(
                    &proposal.leader_mn_id,
                    "Multiple proposals for same transaction",
                );
                warn!(
                    "[{}] ⚠️ Multiple proposals detected for tx {} by leader {}",
                    self.direction, txid_hex, proposal.leader_mn_id
                );
            }

            // §7.6 Week 5-6 Part 3: Verify leader and cast vote
            // Step 1: Compute who the expected leader should be
            let avs = masternodes
                .iter()
                .filter(|mn| mn.is_active)
                .map(|mn| mn.masternode.clone())
                .collect::<Vec<_>>();

            let expected_leader = crate::consensus::compute_fallback_leader(
                &proposal.txid,
                proposal.slot_index,
                &avs,
            );

            // Step 2: Verify the proposal came from the expected leader
            match expected_leader {
                Some(expected_mn_id) if expected_mn_id == proposal.leader_mn_id => {
                    info!(
                        "[{}] ✅ Leader verified: {} is correct leader for slot {}",
                        self.direction, proposal.leader_mn_id, proposal.slot_index
                    );

                    // Step 3: Decide how to vote based on transaction state
                    let vote_decision = consensus.decide_fallback_vote(&proposal.txid);

                    info!(
                        "[{}] Voting {:?} on proposal {} (tx {})",
                        self.direction,
                        vote_decision,
                        hex::encode(proposal_hash),
                        txid_hex
                    );

                    // Step 4: Get our voting weight and broadcast vote
                    // TODO: Get actual voter weight from masternode collateral
                    let voter_weight = 1_000_000_000; // Placeholder: 1 tier weight

                    if let Err(e) = consensus
                        .broadcast_fallback_vote(proposal_hash, vote_decision, voter_weight)
                        .await
                    {
                        warn!("[{}] Failed to broadcast vote: {}", self.direction, e);
                    }
                }
                Some(expected_mn_id) => {
                    warn!(
                        "[{}] ❌ Invalid leader: expected {}, got {} (ignoring proposal)",
                        self.direction, expected_mn_id, proposal.leader_mn_id
                    );
                    // Don't vote on invalid leader proposals
                    return Ok(None);
                }
                None => {
                    warn!(
                        "[{}] ⚠️ Could not compute expected leader (empty AVS?)",
                        self.direction
                    );
                    return Ok(None);
                }
            }
        }

        // Relay the proposal to other peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let _ = broadcast_tx.send(NetworkMessage::FinalityProposal { proposal });
        }

        Ok(None)
    }

    /// Handle FallbackVote message (§7.6.4 Step 4)
    /// AVS member votes on leader's proposal
    async fn handle_fallback_vote(
        &self,
        vote: crate::types::FallbackVote,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let proposal_hex = hex::encode(vote.proposal_hash);

        debug!(
            "[{}] Received FallbackVote for proposal {} from {} (vote: {:?}, weight: {})",
            self.direction, proposal_hex, vote.voter_mn_id, vote.vote, vote.voter_weight
        );

        // Verify the vote signature - find masternode by address
        let masternodes = context.masternode_registry.list_all().await;
        let voter = masternodes
            .iter()
            .find(|mn| mn.masternode.address == vote.voter_mn_id)
            .ok_or_else(|| format!("Voter {} not in masternode registry", vote.voter_mn_id))?;

        vote.verify(&voter.masternode.public_key)
            .map_err(|e| format!("Invalid FallbackVote signature: {}", e))?;

        // §7.6 Week 5-6 Part 2: Accumulate votes and check Q_finality threshold
        if let Some(consensus) = &context.consensus {
            // Phase 4: Detect vote equivocation before processing
            if consensus.detect_vote_equivocation(&vote.proposal_hash, &vote.voter_mn_id) {
                consensus.flag_byzantine(&vote.voter_mn_id, "Vote equivocation detected");
                return Err(format!(
                    "Rejecting vote from {}: equivocation detected",
                    vote.voter_mn_id
                ));
            }

            // Calculate total AVS weight (sum of all masternode sampling weights)
            let total_avs_weight: u64 = masternodes
                .iter()
                .map(|mn| mn.masternode.tier.sampling_weight().max(1))
                .sum();

            // Phase 4: Validate vote weight doesn't exceed total
            if let Err(e) = consensus.validate_vote_weight(&vote.proposal_hash, total_avs_weight) {
                warn!(
                    "[{}] ⚠️ Invalid vote weight for proposal {}: {}",
                    self.direction, proposal_hex, e
                );
            }

            // Accumulate vote and check if quorum reached
            if let Some(decision) =
                consensus.accumulate_fallback_vote(vote.clone(), total_avs_weight)
            {
                // Q_finality threshold reached! Finalize the transaction

                info!(
                    "[{}] 🎯 Q_finality reached for proposal {} (decision: {:?})",
                    self.direction, proposal_hex, decision
                );

                // Get the transaction ID for this proposal
                if let Some(txid) = consensus.get_proposal_txid(&vote.proposal_hash) {
                    let txid_hex = hex::encode(txid);

                    // Calculate total weight that voted for winning decision
                    let (approve_weight, reject_weight, vote_count) = consensus
                        .get_vote_status(&vote.proposal_hash)
                        .unwrap_or((0, 0, 0));

                    let winning_weight = match decision {
                        crate::types::FallbackVoteDecision::Approve => approve_weight,
                        crate::types::FallbackVoteDecision::Reject => reject_weight,
                    };

                    info!(
                        "[{}] Finalizing tx {} via fallback: {:?} (weight: {}/{}, votes: {})",
                        self.direction,
                        txid_hex,
                        decision,
                        winning_weight,
                        total_avs_weight,
                        vote_count
                    );

                    // Finalize the transaction
                    consensus.finalize_from_fallback(txid, decision, winning_weight);
                } else {
                    warn!(
                        "[{}] ⚠️  Quorum reached but no txid mapping for proposal {}",
                        self.direction, proposal_hex
                    );
                }
            } else {
                // Calculate current vote status for logging
                if let Some((approve_weight, reject_weight, vote_count)) =
                    consensus.get_vote_status(&vote.proposal_hash)
                {
                    let q_finality = (total_avs_weight * 2) / 3;
                    debug!(
                        "[{}] Vote accumulated for proposal {}: Approve={}, Reject={}, Total votes={}, Q_finality={}",
                        self.direction,
                        proposal_hex,
                        approve_weight,
                        reject_weight,
                        vote_count,
                        q_finality
                    );
                }
            }
        }

        // Relay the vote to other peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let _ = broadcast_tx.send(NetworkMessage::FallbackVote { vote });
        }

        Ok(None)
    }

    /// CRITICAL SECURITY: Validate block before voting to prevent consensus on invalid blocks
    ///
    /// This validation must happen BEFORE voting to ensure:
    /// - Invalid blocks don't accumulate votes
    /// - Network doesn't waste resources on invalid proposals
    /// - Consensus can't finalize blocks that will be rejected during add_block()
    async fn validate_block_before_vote(
        &self,
        block: &Block,
        context: &MessageContext,
    ) -> Result<(), String> {
        // 1. Validate block structure and size
        let serialized =
            bincode::serialize(block).map_err(|e| format!("Failed to serialize block: {}", e))?;

        const MAX_BLOCK_SIZE: usize = 4 * 1024 * 1024; // 4MB
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!(
                "Block too large: {} bytes (max {})",
                serialized.len(),
                MAX_BLOCK_SIZE
            ));
        }

        // 2. Validate merkle root
        let computed_merkle = calculate_merkle_root(&block.transactions);
        if block.header.merkle_root != computed_merkle {
            return Err(format!(
                "Invalid merkle root: expected {}, got {}",
                hex::encode(computed_merkle),
                hex::encode(block.header.merkle_root)
            ));
        }

        // 3. Validate block must have at least 2 transactions (coinbase + reward_distribution)
        if block.transactions.len() < 2 {
            return Err(format!(
                "Block has {} transactions, expected at least 2",
                block.transactions.len()
            ));
        }

        // 4. Validate block rewards (prevents double-counting and inflation)
        // Skip for genesis block
        if block.header.height > 0 {
            self.validate_block_rewards_structure(block)?;
        }

        // 5. SECURITY: Verify VRF proof — confirms proposer is legitimately selected
        // Skip for old blocks without VRF proof (backward compatibility)
        if !block.header.vrf_proof.is_empty() && block.header.height > 0 {
            // Look up the proposer's public key from masternode registry
            let proposer = block.header.leader.clone();
            if proposer.is_empty() {
                return Err("Block has VRF proof but no leader set".to_string());
            }

            let proposer_info = context
                .masternode_registry
                .get(&proposer)
                .await
                .ok_or_else(|| {
                    format!(
                        "Block proposer {} not found in masternode registry",
                        proposer
                    )
                })?;

            // Anti-sybil: reject blocks from immature Free-tier proposers
            if !crate::masternode_registry::MasternodeRegistry::is_mature_for_sortition(
                &proposer_info,
                block.header.height,
                context.masternode_registry.network(),
            ) {
                return Err(format!(
                    "Block proposer {} is an immature Free-tier node (registered at height {}, current {})",
                    proposer, proposer_info.registration_height, block.header.height
                ));
            }

            // Verify the VRF proof using the proposer's public key
            crate::block::vrf::verify_block_vrf(
                &proposer_info.masternode.public_key,
                block.header.height,
                &block.header.previous_hash,
                &block.header.vrf_proof,
                &block.header.vrf_output,
            )?;

            // Verify vrf_score matches vrf_output
            let expected_score = crate::block::vrf::vrf_output_to_score(&block.header.vrf_output);
            if block.header.vrf_score != expected_score {
                return Err(format!(
                    "VRF score mismatch: header={}, computed={}",
                    block.header.vrf_score, expected_score
                ));
            }

            // Verify the proposer's VRF score qualifies them (sampling weight + fairness bonus)
            let blocks_without_reward_map = context
                .masternode_registry
                .get_verifiable_reward_tracking(&context.blockchain)
                .await;

            let proposer_blocks_without = blocks_without_reward_map
                .get(&proposer)
                .copied()
                .unwrap_or(0);
            let proposer_fairness_bonus = (proposer_blocks_without / 10).min(20);
            let proposer_weight =
                proposer_info.masternode.tier.sampling_weight() + proposer_fairness_bonus;

            let eligible_masternodes = context
                .masternode_registry
                .get_vrf_eligible(block.header.height)
                .await;
            let total_sampling_weight: u64 = eligible_masternodes
                .iter()
                .map(|(mn, _)| {
                    let bonus = blocks_without_reward_map
                        .get(&mn.address)
                        .copied()
                        .map(|b| (b / 10).min(20))
                        .unwrap_or(0);
                    mn.tier.sampling_weight() + bonus
                })
                .sum();

            if total_sampling_weight > 0 {
                let is_eligible = crate::block::vrf::vrf_check_proposer_eligible(
                    block.header.vrf_score,
                    proposer_weight,
                    total_sampling_weight,
                );

                if !is_eligible {
                    // Allow relaxed threshold during timeout (same exponential backoff)
                    // Check if we've been waiting for this height
                    let our_height = context.blockchain.get_height();
                    let expected_height = our_height + 1;
                    if block.header.height == expected_height {
                        // Check how long since the slot started
                        let genesis_ts = context.blockchain.genesis_timestamp();
                        let slot_time = genesis_ts + (block.header.height as i64 * 600);
                        let now = chrono::Utc::now().timestamp();
                        let elapsed = (now - slot_time).max(0) as u64;
                        let timeout_attempts = elapsed / 10; // Same 10s timeout as producer

                        if timeout_attempts > 0 {
                            let multiplier = 1u64 << timeout_attempts.min(20);
                            let relaxed_weight = proposer_weight
                                .saturating_mul(multiplier)
                                .min(total_sampling_weight);
                            let eligible_relaxed = crate::block::vrf::vrf_check_proposer_eligible(
                                block.header.vrf_score,
                                relaxed_weight,
                                total_sampling_weight,
                            );
                            if !eligible_relaxed {
                                return Err(format!(
                                    "Proposer {} VRF score {} exceeds threshold (even with {}x relaxation)",
                                    proposer, block.header.vrf_score, multiplier
                                ));
                            }
                            debug!(
                                "🎲 [{}] Block {} proposer {} accepted with relaxed VRF threshold (attempt {})",
                                self.direction, block.header.height, proposer, timeout_attempts
                            );
                        } else {
                            return Err(format!(
                                "Proposer {} VRF score {} exceeds threshold (weight {}/{})",
                                proposer,
                                block.header.vrf_score,
                                proposer_weight,
                                total_sampling_weight
                            ));
                        }
                    }
                }
            }

            debug!(
                "🎲 [{}] Block {} VRF verified: proposer={}, score={}",
                self.direction, block.header.height, proposer, block.header.vrf_score
            );
        }

        // 7. Get consensus engine for transaction validation
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // 8. Validate all transactions (except coinbase and reward distribution)
        // Transactions 0-1 are system transactions (coinbase + reward_distribution)
        // Transactions 2+ are user transactions that need full validation
        for (idx, tx) in block.transactions.iter().enumerate() {
            if idx < 2 {
                continue; // Skip coinbase and reward distribution (validated separately)
            }

            // Validate transaction structure and signatures
            if let Err(e) = consensus.validate_transaction(tx).await {
                return Err(format!("Invalid transaction at index {}: {}", idx, e));
            }
        }

        // 9. Check for double-spends within the block
        let mut spent_in_block = std::collections::HashSet::new();
        for (idx, tx) in block.transactions.iter().enumerate() {
            for input in &tx.inputs {
                let outpoint_key = format!(
                    "{}:{}",
                    hex::encode(input.previous_output.txid),
                    input.previous_output.vout
                );
                if spent_in_block.contains(&outpoint_key) {
                    return Err(format!(
                        "Double-spend detected in block: UTXO {} spent multiple times",
                        outpoint_key
                    ));
                }
                spent_in_block.insert(outpoint_key);
            }

            // Also check if attempting to spend outputs created in same block
            // This is allowed (chained transactions) but needs careful tracking
            debug!(
                "Transaction {} spends {} inputs, creates {} outputs",
                idx,
                tx.inputs.len(),
                tx.outputs.len()
            );
        }

        info!(
            "✅ [{}] Block {} validation passed: {} transactions, {} bytes",
            self.direction,
            block.header.height,
            block.transactions.len(),
            serialized.len()
        );

        Ok(())
    }

    /// Validate block reward structure (similar to blockchain.rs validation)
    fn validate_block_rewards_structure(&self, block: &Block) -> Result<(), String> {
        // Transaction 0 should be coinbase
        let coinbase = &block.transactions[0];
        if !coinbase.inputs.is_empty() {
            return Err(format!(
                "Coinbase has {} inputs, expected 0",
                coinbase.inputs.len()
            ));
        }

        if coinbase.outputs.len() != 1 {
            return Err(format!(
                "Coinbase has {} outputs, expected 1",
                coinbase.outputs.len()
            ));
        }

        let coinbase_amount = coinbase.outputs[0].value;
        if coinbase_amount != block.header.block_reward {
            return Err(format!(
                "Coinbase creates {} satoshis, but block_reward is {}",
                coinbase_amount, block.header.block_reward
            ));
        }

        // Transaction 1 should be reward distribution
        let reward_dist = &block.transactions[1];

        if reward_dist.inputs.len() != 1 {
            return Err(format!(
                "Reward distribution has {} inputs, expected 1",
                reward_dist.inputs.len()
            ));
        }

        let coinbase_txid = coinbase.txid();
        if reward_dist.inputs[0].previous_output.txid != coinbase_txid {
            return Err("Reward distribution doesn't spend coinbase".to_string());
        }

        if reward_dist.outputs.len() != block.masternode_rewards.len() {
            return Err(format!(
                "Reward distribution has {} outputs but masternode_rewards has {} entries",
                reward_dist.outputs.len(),
                block.masternode_rewards.len()
            ));
        }

        // Verify total outputs match block reward exactly (with small tolerance for rounding)
        let total_distributed: u64 = reward_dist.outputs.iter().map(|o| o.value).sum();
        let expected_total = block.header.block_reward;

        // Allow small tolerance for rounding errors in integer division
        // Tolerance should be less than the number of masternodes (worst case: 1 satoshi per node)
        let tolerance = block.masternode_rewards.len() as u64;

        let lower_bound = expected_total.saturating_sub(tolerance);
        let upper_bound = expected_total;

        if total_distributed < lower_bound || total_distributed > upper_bound {
            return Err(format!(
                "Total distributed {} outside valid range {}-{} (block_reward: {})",
                total_distributed, lower_bound, upper_bound, expected_total
            ));
        }

        Ok(())
    }

    /// Handle UTXOStateUpdate - CRITICAL for instant finality
    /// When a node locks a UTXO for a transaction, it broadcasts the lock.
    /// All other nodes MUST respect this lock to prevent double-spends.
    async fn handle_utxo_state_update(
        &self,
        outpoint: OutPoint,
        state: UTXOState,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        tracing::debug!(
            "🔒 [{}] Received UTXO state update for {} -> {:?}",
            self.direction,
            outpoint,
            state
        );

        // Apply the state update from remote node
        if let Some(consensus) = &context.consensus {
            consensus
                .utxo_manager
                .update_state(&outpoint, state.clone());

            // Log important state changes
            match state {
                UTXOState::Locked { txid, .. } => {
                    tracing::info!(
                        "🔒 [{}] Locked UTXO {} for TX {}",
                        self.direction,
                        outpoint,
                        hex::encode(txid)
                    );
                }
                UTXOState::SpentPending { txid, .. } | UTXOState::SpentFinalized { txid, .. } => {
                    tracing::info!(
                        "💸 [{}] Marked UTXO {} as spent by TX {}",
                        self.direction,
                        outpoint,
                        hex::encode(txid)
                    );
                }
                _ => {}
            }
        }

        Ok(None)
    }

    // === TimeVote Consensus Handlers ===

    async fn handle_timevote_request(
        &self,
        txid: [u8; 32],
        tx_hash_commitment: [u8; 32],
        slot_index: u64,
        tx_from_request: Option<crate::types::Transaction>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let consensus = context
            .consensus
            .as_ref()
            .ok_or("No consensus engine available for TimeVoteRequest")?;

        tracing::info!(
            "🗳️  TimeVoteRequest from {} for TX {:?} (slot {}){}",
            self.peer_ip,
            hex::encode(txid),
            slot_index,
            if tx_from_request.is_some() {
                " [TX included]"
            } else {
                ""
            }
        );

        // Step 1: Get TX from mempool or from request
        let mut tx_opt = consensus.tx_pool.get_pending(&txid);

        if tx_opt.is_none() {
            if let Some(tx_from_req) = tx_from_request {
                let input_sum: u64 = {
                    let mut sum = 0u64;
                    for input in &tx_from_req.inputs {
                        if let Ok(utxo) = consensus
                            .utxo_manager
                            .get_utxo(&input.previous_output)
                            .await
                        {
                            sum += utxo.value;
                        }
                    }
                    sum
                };
                let output_sum: u64 = tx_from_req.outputs.iter().map(|o| o.value).sum();
                let fee = input_sum.saturating_sub(output_sum);

                if consensus
                    .tx_pool
                    .add_pending(tx_from_req.clone(), fee)
                    .is_ok()
                {
                    tx_opt = Some(tx_from_req);
                }
            }
        }

        let decision = if let Some(tx) = tx_opt {
            let actual_commitment = crate::types::TimeVote::calculate_tx_commitment(&tx);
            if actual_commitment != tx_hash_commitment {
                tracing::warn!("⚠️  TX {:?} commitment mismatch", hex::encode(txid));
                crate::types::VoteDecision::Reject
            } else {
                match consensus.validate_transaction(&tx).await {
                    Ok(_) => {
                        tracing::info!(
                            "✅ TX {:?} validated successfully for vote",
                            hex::encode(txid)
                        );
                        crate::types::VoteDecision::Accept
                    }
                    Err(e) => {
                        tracing::warn!("⚠️  TX {:?} validation failed: {}", hex::encode(txid), e);
                        crate::types::VoteDecision::Reject
                    }
                }
            }
        } else {
            tracing::debug!(
                "⚠️  TX {:?} not found in mempool and not included in request",
                hex::encode(txid)
            );
            crate::types::VoteDecision::Reject
        };

        // Sign TimeVote with our masternode key
        let vote_opt = consensus.sign_timevote(txid, tx_hash_commitment, slot_index, decision);

        if let Some(vote) = vote_opt {
            tracing::info!(
                "✅ TimeVoteResponse ready for TX {:?} (decision: {:?})",
                hex::encode(txid),
                decision
            );
            Ok(Some(NetworkMessage::TimeVoteResponse { vote }))
        } else {
            tracing::warn!(
                "⚠️ TimeVote signing skipped for TX {:?} (not a masternode or identity not set)",
                hex::encode(txid)
            );
            Ok(None)
        }
    }

    async fn handle_timevote_response(
        &self,
        vote: crate::types::TimeVote,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let consensus = context
            .consensus
            .as_ref()
            .ok_or("No consensus engine available for TimeVoteResponse")?;

        let txid = vote.txid;

        tracing::info!(
            "📥 TimeVoteResponse from {} for TX {:?} (decision: {:?}, weight: {})",
            self.peer_ip,
            hex::encode(txid),
            vote.decision,
            vote.voter_weight
        );

        // Step 1: Accumulate the vote
        let accumulated_weight = match consensus.timevote.accumulate_timevote(vote) {
            Ok(weight) => weight,
            Err(e) => {
                tracing::warn!(
                    "Failed to accumulate vote for TX {:?}: {}",
                    hex::encode(txid),
                    e
                );
                return Ok(None);
            }
        };

        tracing::info!(
            "Vote accumulated for TX {:?}, total weight: {}",
            hex::encode(txid),
            accumulated_weight
        );

        // Step 2: Check if finality threshold reached (51% simple majority)
        let validators = consensus.timevote.get_validators();
        let total_avs_weight: u64 = validators.iter().map(|v| v.weight).sum();
        let finality_threshold = ((total_avs_weight as f64) * 0.51).ceil() as u64;

        tracing::info!(
            "Finality check for TX {:?}: accumulated={}, threshold={} (51% of {})",
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

            use dashmap::mapref::entry::Entry;
            match consensus.timevote.finalized_txs.entry(txid) {
                Entry::Vacant(e) => {
                    e.insert((
                        crate::consensus::Preference::Accept,
                        std::time::Instant::now(),
                    ));

                    if consensus.tx_pool.finalize_transaction(txid) {
                        tracing::info!("✅ TX {:?} moved to finalized pool", hex::encode(txid));

                        consensus
                            .timevote
                            .record_finalization(txid, accumulated_weight);

                        match consensus.timevote.assemble_timeproof(txid) {
                            Ok(timeproof) => {
                                tracing::info!(
                                    "📜 TimeProof assembled for TX {:?} with {} votes",
                                    hex::encode(txid),
                                    timeproof.votes.len()
                                );

                                if let Err(e) = consensus
                                    .finality_proof_mgr
                                    .store_timeproof(timeproof.clone())
                                {
                                    tracing::error!(
                                        "❌ Failed to store TimeProof for TX {:?}: {}",
                                        hex::encode(txid),
                                        e
                                    );
                                }

                                consensus.broadcast_timeproof(timeproof).await;
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
                Entry::Occupied(_) => {
                    tracing::debug!(
                        "TX {:?} already finalized by another task",
                        hex::encode(txid)
                    );
                }
            }
        }

        Ok(None)
    }

    async fn handle_timeproof_broadcast(
        &self,
        proof: crate::types::TimeProof,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let consensus = context
            .consensus
            .as_ref()
            .ok_or("No consensus engine available for TimeProofBroadcast")?;

        tracing::info!(
            "📜 Received TimeProof from {} for TX {:?} with {} votes",
            self.peer_ip,
            hex::encode(proof.txid),
            proof.votes.len()
        );

        match consensus.timevote.verify_timeproof(&proof) {
            Ok(_) => {
                tracing::info!("✅ TimeProof verified for TX {:?}", hex::encode(proof.txid));

                if let Err(e) = consensus.finality_proof_mgr.store_timeproof(proof) {
                    tracing::error!("❌ Failed to store TimeProof: {}", e);
                } else {
                    tracing::info!("💾 TimeProof stored successfully");
                }
            }
            Err(e) => {
                tracing::warn!("⚠️  Invalid TimeProof from {}: {}", self.peer_ip, e);
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_direction_display() {
        assert_eq!(format!("{}", ConnectionDirection::Inbound), "Inbound");
        assert_eq!(format!("{}", ConnectionDirection::Outbound), "Outbound");
    }

    #[test]
    fn test_message_handler_new() {
        let handler = MessageHandler::new("127.0.0.1".to_string(), ConnectionDirection::Inbound);
        assert_eq!(handler.peer_ip, "127.0.0.1");
        assert_eq!(handler.direction, ConnectionDirection::Inbound);
    }
}
