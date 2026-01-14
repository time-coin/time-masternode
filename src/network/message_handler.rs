//! Unified message handler for both inbound and outbound connections
//!
//! This module provides a single implementation of network message handling
//! that works regardless of connection direction. Previously, message handling
//! was duplicated between server.rs (inbound) and peer_connection.rs (outbound).

use crate::block::types::Block;
use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::heartbeat_attestation::HeartbeatAttestationSystem;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::dedup_filter::DeduplicationFilter;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::utxo_manager::UTXOStateManager;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

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
    pub attestation_system: Option<Arc<HeartbeatAttestationSystem>>,
    pub seen_blocks: Option<Arc<DeduplicationFilter>>,
    pub seen_transactions: Option<Arc<DeduplicationFilter>>,
    // Node identity for voting
    pub node_masternode_address: Option<String>,
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
            attestation_system: None,
            seen_blocks: None,
            seen_transactions: None,
            node_masternode_address: None,
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
            attestation_system: None,
            seen_blocks: None,
            seen_transactions: None,
            node_masternode_address: None,
        }
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
}

impl MessageHandler {
    /// Create a new message handler for a specific peer and connection direction
    pub fn new(peer_ip: String, direction: ConnectionDirection) -> Self {
        Self {
            peer_ip,
            direction,
            recent_requests: Arc::new(RwLock::new(Vec::new())),
        }
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
        match msg {
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
            NetworkMessage::MasternodeAnnouncement {
                address,
                reward_address,
                tier,
                public_key,
            } => {
                self.handle_masternode_announcement(
                    address.clone(),
                    reward_address.clone(),
                    *tier,
                    *public_key,
                    context,
                )
                .await
            }
            NetworkMessage::MasternodesResponse(masternodes) => {
                self.handle_masternodes_response(masternodes.clone(), context)
                    .await
            }

            // === Heartbeat Messages ===
            NetworkMessage::HeartbeatBroadcast(heartbeat) => {
                self.handle_heartbeat_broadcast(heartbeat.clone(), context)
                    .await
            }
            NetworkMessage::HeartbeatAttestation(attestation) => {
                self.handle_heartbeat_attestation(attestation.clone(), context)
                    .await
            }

            // === UTXO Messages ===
            NetworkMessage::UTXOStateQuery(outpoints) => {
                self.handle_utxo_state_query(outpoints.clone(), context)
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

            // === TSDC Consensus Messages ===
            NetworkMessage::TSCDBlockProposal { block } => {
                self.handle_tsdc_block_proposal(block.clone(), context)
                    .await
            }
            NetworkMessage::TSCDPrepareVote {
                block_hash,
                voter_id,
                signature,
            } => {
                self.handle_tsdc_prepare_vote(
                    *block_hash,
                    voter_id.clone(),
                    signature.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::TSCDPrecommitVote {
                block_hash,
                voter_id,
                signature,
            } => {
                self.handle_tsdc_precommit_vote(
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

            // === Response Messages (handled by caller) ===
            NetworkMessage::BlockHeightResponse(_)
            | NetworkMessage::ChainTipResponse { .. }
            | NetworkMessage::BlocksResponse(_)
            | NetworkMessage::BlockRangeResponse(_)
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
        }
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
        info!(
            "📥 [{}] Received GetBlocks({}-{}) from {} (our height: {})",
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
        let effective_end = end.min(start + 100).min(our_height);

        if start <= our_height {
            let mut missing_blocks = Vec::new();
            for h in start..=effective_end {
                match context.blockchain.get_block_by_height(h).await {
                    Ok(block) => blocks.push(block),
                    Err(e) => {
                        warn!(
                            "⚠️ [{}] Failed to retrieve block {} for {}: {}",
                            self.direction, h, self.peer_ip, e
                        );
                        missing_blocks.push(h);
                    }
                }
            }

            if blocks.is_empty() && start <= our_height {
                warn!(
                    "⚠️ [{}] No blocks available to send to {} (requested {}-{}, our height: {}, missing: {:?})",
                    self.direction, self.peer_ip, start, end, our_height, missing_blocks
                );
            } else {
                info!(
                    "📤 [{}] Sending {} blocks to {} (requested {}-{}, effective {}-{}, missing: {})",
                    self.direction,
                    blocks.len(),
                    self.peer_ip,
                    start,
                    end,
                    start,
                    effective_end,
                    missing_blocks.len()
                );
            }
        } else {
            // Requested blocks are beyond our height - we don't have them yet
            info!(
                "⏭️  [{}] Cannot send blocks {}-{} to {} - we only have up to height {}",
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
                }
            })
            .collect();

        info!(
            "📤 [{}] Responded with {} masternode(s) to {}",
            self.direction,
            all_masternodes.len(),
            self.peer_ip
        );

        Ok(Some(NetworkMessage::MasternodesResponse(mn_data)))
    }

    /// Handle TSDC Block Proposal - cache and vote
    async fn handle_tsdc_block_proposal(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_height = block.header.height;

        info!(
            "📦 [{}] Received TSDC block proposal at height {} from {}",
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

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

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
            Some(info) => info.masternode.collateral.max(1),
            None => 1u64, // Default to 1 if not found
        };

        consensus
            .avalanche
            .generate_prepare_vote(block_hash, &validator_id, validator_weight);
        info!(
            "✅ [{}] Generated prepare vote for block {} at height {}",
            self.direction,
            hex::encode(block_hash),
            block.header.height
        );

        // Broadcast prepare vote to all peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let sig_bytes = vec![]; // TODO: Phase 3E.4: Sign with validator key
            let prepare_vote = NetworkMessage::TSCDPrepareVote {
                block_hash,
                voter_id: validator_id.clone(),
                signature: sig_bytes,
            };

            match broadcast_tx.send(prepare_vote) {
                Ok(receivers) => {
                    info!(
                        "📤 [{}] Broadcast prepare vote to {} peers",
                        self.direction,
                        receivers.saturating_sub(1)
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

    /// Handle TSDC Prepare Vote - accumulate and check consensus
    async fn handle_tsdc_prepare_vote(
        &self,
        block_hash: [u8; 32],
        voter_id: String,
        _signature: Vec<u8>,
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
        let voter_weight = match context.masternode_registry.get(&voter_id).await {
            Some(info) => info.masternode.collateral,
            None => 1u64, // Default to 1 if not found
        };

        // Phase 3E.4: Verify vote signature (stub - TODO: implement Ed25519 verification)
        // For now, we accept the vote; in production, verify the signature

        consensus
            .avalanche
            .accumulate_prepare_vote(block_hash, voter_id.clone(), voter_weight);

        // Check if prepare consensus reached (>50% majority Avalanche)
        if consensus.avalanche.check_prepare_consensus(block_hash) {
            info!(
                "✅ [{}] Prepare consensus reached for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Generate precommit vote with actual weight
            let validator_id = context
                .node_masternode_address
                .clone()
                .unwrap_or_else(|| format!("node_{}", self.peer_ip));
            let validator_weight = match context.masternode_registry.get(&validator_id).await {
                Some(info) => info.masternode.collateral.max(1),
                None => 1u64,
            };

            consensus.avalanche.generate_precommit_vote(
                block_hash,
                &validator_id,
                validator_weight,
            );
            info!(
                "✅ [{}] Generated precommit vote for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Broadcast precommit vote
            if let Some(broadcast_tx) = &context.broadcast_tx {
                let precommit_vote = NetworkMessage::TSCDPrecommitVote {
                    block_hash,
                    voter_id: validator_id,
                    signature: vec![],
                };

                match broadcast_tx.send(precommit_vote) {
                    Ok(_) => {
                        debug!("[{}] Broadcast precommit vote", self.direction);
                    }
                    Err(_) => {
                        debug!(
                            "[{}] No active peers to broadcast precommit vote (channel closed)",
                            self.direction
                        );
                    }
                }
            }
        }

        Ok(None)
    }

    /// Handle TSDC Precommit Vote - accumulate and finalize if consensus reached
    async fn handle_tsdc_precommit_vote(
        &self,
        block_hash: [u8; 32],
        voter_id: String,
        _signature: Vec<u8>,
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
        let voter_weight = match context.masternode_registry.get(&voter_id).await {
            Some(info) => info.masternode.collateral,
            None => 1u64, // Default to 1 if not found
        };

        // Phase 3E.4: Verify vote signature (stub)
        // In production, verify Ed25519 signature here

        consensus
            .avalanche
            .accumulate_precommit_vote(block_hash, voter_id.clone(), voter_weight);

        // Check if precommit consensus reached (>50% majority Avalanche)
        if consensus.avalanche.check_precommit_consensus(block_hash) {
            info!(
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
                    //    let signatures = consensus.avalanche.get_precommit_signatures(block_hash)?;
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

                    // 3. Phase 3E.3: Call tsdc.finalize_block_complete()
                    // Note: This would be called through a TSDC module instance
                    // For now, emit finalization event
                    info!(
                        "🎉 [{}] Block {} finalized with consensus!",
                        self.direction,
                        hex::encode(block_hash)
                    );
                    info!(
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

                    info!(
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
                        if let Err(e) = context.blockchain.add_block(block).await {
                            // Check if this is a height mismatch (gap) error
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

                                let sync_msg =
                                    NetworkMessage::GetBlocks(current_height + 1, block_height - 1);

                                if let Err(send_err) = context
                                    .peer_registry
                                    .send_to_peer(&self.peer_ip, sync_msg)
                                    .await
                                {
                                    warn!("Failed to request missing blocks: {}", send_err);
                                }
                            } else {
                                warn!(
                                    "[{}] Failed to add finalized block to blockchain: {}",
                                    self.direction, e
                                );
                            }
                        } else {
                            info!(
                                "✅ [{}] Added finalized block {} to blockchain",
                                self.direction,
                                hex::encode(block_hash)
                            );
                        }
                    } else {
                        debug!(
                            "[{}] Block {} already in blockchain at height {}, skipping add",
                            self.direction,
                            hex::encode(block_hash),
                            block.header.height
                        );
                    }
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

    /// Handle FinalityVoteBroadcast - verify signature and accumulate vote
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
            if let Err(e) = consensus.avalanche.accumulate_finality_vote(vote) {
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
        let hash = context
            .blockchain
            .get_block_hash(height)
            .unwrap_or([0u8; 32]);
        info!(
            "📥 [{}] Received GetChainTip from {}, responding with height {} hash {}",
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

                // Gossip inventory to other peers
                if let Some(broadcast_tx) = &context.broadcast_tx {
                    let msg = NetworkMessage::BlockInventory(block_height);
                    if let Ok(receivers) = broadcast_tx.send(msg) {
                        debug!(
                            "🔄 [{}] Gossiped block {} inventory to {} peer(s)",
                            self.direction,
                            block_height,
                            receivers.saturating_sub(1)
                        );
                    }
                }
            }
            Ok(false) => {
                debug!(
                    "⏭️ [{}] Skipped block {} (already have or invalid)",
                    self.direction, block_height
                );
            }
            Err(e) => {
                warn!(
                    "❌ [{}] Failed to add block {}: {}",
                    self.direction, block_height, e
                );
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
                                self.direction,
                                receivers.saturating_sub(1)
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
        address: String,
        reward_address: String,
        tier: crate::types::MasternodeTier,
        public_key: ed25519_dalek::VerifyingKey,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Use peer IP instead of announced address for security
        let peer_ip = self.peer_ip.clone();

        info!(
            "📨 [{}] Received masternode announcement from {} (announced: {})",
            self.direction, peer_ip, address
        );

        let mn = crate::types::Masternode {
            address: peer_ip.clone(),
            wallet_address: reward_address.clone(),
            collateral: tier.collateral(),
            tier,
            public_key,
            registered_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        match context
            .masternode_registry
            .register(mn, reward_address)
            .await
        {
            Ok(()) => {
                let count = context.masternode_registry.total_count().await;
                info!(
                    "✅ [{}] Registered masternode {} (total: {})",
                    self.direction, peer_ip, count
                );

                // Add to peer_manager for connections
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

        Ok(None)
    }

    /// Handle MasternodesResponse
    async fn handle_masternodes_response(
        &self,
        masternodes: Vec<crate::network::message::MasternodeAnnouncementData>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received MasternodesResponse from {} with {} masternode(s)",
            self.direction,
            self.peer_ip,
            masternodes.len()
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut registered = 0;
        for mn_data in masternodes {
            let masternode = crate::types::Masternode {
                address: mn_data.address.clone(),
                wallet_address: mn_data.reward_address.clone(),
                tier: mn_data.tier,
                public_key: mn_data.public_key,
                collateral: 0,
                registered_at: now,
            };

            if context
                .masternode_registry
                .register_internal(masternode, mn_data.reward_address, false)
                .await
                .is_ok()
            {
                registered += 1;
            }
        }

        if registered > 0 {
            info!(
                "✓ [{}] Registered {} masternode(s) from peer exchange",
                self.direction, registered
            );
        }

        Ok(None)
    }

    /// Handle HeartbeatBroadcast
    async fn handle_heartbeat_broadcast(
        &self,
        heartbeat: crate::heartbeat_attestation::SignedHeartbeat,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "💓 [{}] Received heartbeat from {} (height {})",
            self.direction, heartbeat.masternode_address, heartbeat.block_height
        );

        // Check for opportunistic sync
        let our_height = context.blockchain.get_height();
        if heartbeat.block_height > our_height {
            info!(
                "🔄 [{}] Opportunistic sync: peer {} at height {} (we're at {})",
                self.direction, heartbeat.masternode_address, heartbeat.block_height, our_height
            );

            // Request blocks from this peer
            let start_height = our_height + 1;
            let request = NetworkMessage::GetBlocks(start_height, heartbeat.block_height);

            if let Err(e) = context
                .peer_registry
                .send_to_peer(&heartbeat.masternode_address, request)
                .await
            {
                debug!(
                    "⚠️ [{}] Failed to request blocks from {}: {}",
                    self.direction, heartbeat.masternode_address, e
                );
            }
        }

        // Process heartbeat through masternode registry
        if let Err(e) = context
            .masternode_registry
            .receive_heartbeat_broadcast(heartbeat.clone(), None)
            .await
        {
            debug!("⚠️ [{}] Failed to process heartbeat: {}", self.direction, e);
        }

        // Process through attestation system if available
        if let Some(attestation_system) = &context.attestation_system {
            if let Ok(Some(attestation)) = attestation_system
                .receive_heartbeat(heartbeat.clone())
                .await
            {
                // Broadcast attestation
                if let Some(broadcast_tx) = &context.broadcast_tx {
                    let msg = NetworkMessage::HeartbeatAttestation(attestation);
                    let _ = broadcast_tx.send(msg);
                }
            }
        }

        // Re-broadcast heartbeat to other peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let msg = NetworkMessage::HeartbeatBroadcast(heartbeat);
            let _ = broadcast_tx.send(msg);
        }

        Ok(None)
    }

    /// Handle HeartbeatAttestation
    async fn handle_heartbeat_attestation(
        &self,
        attestation: crate::heartbeat_attestation::WitnessAttestation,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📝 [{}] Received heartbeat attestation from {}",
            self.direction, attestation.witness_address
        );

        // Add attestation to the attestation system
        if let Some(attestation_system) = &context.attestation_system {
            if let Err(e) = attestation_system
                .add_attestation(attestation.clone())
                .await
            {
                debug!("⚠️ [{}] Failed to add attestation: {}", self.direction, e);
            }
        }

        // Process through masternode registry
        if let Err(e) = context
            .masternode_registry
            .receive_attestation_broadcast(attestation.clone())
            .await
        {
            debug!(
                "⚠️ [{}] Failed to process attestation: {}",
                self.direction, e
            );
        }

        // Re-broadcast to other peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let msg = NetworkMessage::HeartbeatAttestation(attestation);
            let _ = broadcast_tx.send(msg);
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

        // If we're on the minority fork, request consensus chain
        if your_height == consensus_height && your_hash != consensus_hash {
            warn!("   ⚠️ We appear to be on minority fork! Requesting consensus chain...");
            let request_from = consensus_height.saturating_sub(10);
            return Ok(Some(NetworkMessage::GetBlocks(
                request_from,
                consensus_height + 5,
            )));
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
