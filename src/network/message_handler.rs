//! Unified message handler for both inbound and outbound connections
//!
//! This module provides a single implementation of network message handling
//! that works regardless of connection direction. Previously, message handling
//! was duplicated between server.rs (inbound) and peer_connection.rs (outbound).

use crate::block::types::Block;
use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
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
    #[allow(dead_code)]
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub masternode_registry: Arc<MasternodeRegistry>,
    #[allow(dead_code)]
    pub consensus: Option<Arc<ConsensusEngine>>,
    pub block_cache: Option<Arc<crate::network::block_cache::BlockCache>>,
    pub broadcast_tx: Option<broadcast::Sender<NetworkMessage>>,
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
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height: _,
            } => self.handle_ping(*nonce, *timestamp).await,
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height: _,
            } => self.handle_pong(*nonce, *timestamp).await,
            NetworkMessage::GetBlocks(start, end) => {
                self.handle_get_blocks(*start, *end, context).await
            }
            NetworkMessage::GetMasternodes => self.handle_get_masternodes(context).await,
            NetworkMessage::BlockHeightResponse(_) => {
                // Handled by caller (no response needed)
                Ok(None)
            }
            NetworkMessage::BlocksResponse(_) | NetworkMessage::BlockRangeResponse(_) => {
                // Handled by caller (no response needed)
                Ok(None)
            }
            NetworkMessage::MasternodesResponse(_) => {
                // Handled by caller (no response needed)
                Ok(None)
            }
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
            _ => {
                debug!(
                    "[{}] Unhandled message type from {}",
                    self.direction, self.peer_ip
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
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📨 [{}] Received ping from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        // Phase 3: MessageHandler doesn't have blockchain access, so we can't include height
        // This is fine - height will be included in peer_connection and server handlers
        let pong = NetworkMessage::Pong {
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            height: None, // Phase 3: No blockchain access in this handler
        };

        info!(
            "✅ [{}] Sent pong to {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        Ok(Some(pong))
    }

    /// Handle Pong message - no response needed
    async fn handle_pong(
        &self,
        nonce: u64,
        _timestamp: i64,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📨 [{}] Received pong from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );
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

        // Phase 3E.2: Look up validator weight from masternode registry
        let validator_id = "validator_node".to_string();
        let validator_weight = match context.masternode_registry.get(&validator_id).await {
            Some(info) => info.masternode.collateral,
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
            let validator_id = "validator_node".to_string();
            let validator_weight = match context.masternode_registry.get(&validator_id).await {
                Some(info) => info.masternode.collateral,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ping_pong() {
        let handler = MessageHandler::new("127.0.0.1".to_string(), ConnectionDirection::Inbound);

        let nonce = 12345u64;
        let timestamp = chrono::Utc::now().timestamp();

        let result = handler.handle_ping(nonce, timestamp).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.is_some());

        if let Some(NetworkMessage::Pong {
            nonce: pong_nonce, ..
        }) = response
        {
            assert_eq!(pong_nonce, nonce);
        } else {
            panic!("Expected Pong response");
        }
    }
}
