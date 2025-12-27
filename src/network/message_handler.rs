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
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
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
    pub block_cache: Option<Arc<DashMap<[u8; 32], Block>>>,
    pub broadcast_tx: Option<broadcast::Sender<NetworkMessage>>,
}

/// Unified message handler for all network messages
pub struct MessageHandler {
    peer_ip: String,
    direction: ConnectionDirection,
}

impl MessageHandler {
    /// Create a new message handler for a specific peer and connection direction
    pub fn new(peer_ip: String, direction: ConnectionDirection) -> Self {
        Self { peer_ip, direction }
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
            NetworkMessage::Ping { nonce, timestamp } => self.handle_ping(*nonce, *timestamp).await,
            NetworkMessage::Pong { nonce, timestamp } => self.handle_pong(*nonce, *timestamp).await,
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

        let pong = NetworkMessage::Pong {
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
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
        let our_height = context.blockchain.get_height().await;
        info!(
            "📥 [{}] Received GetBlocks({}-{}) from {} (our height: {})",
            self.direction, start, end, self.peer_ip, our_height
        );

        let mut blocks = Vec::new();
        // Send blocks we have: cap at our_height, requested end, and batch limit of 100
        let effective_end = end.min(start + 100).min(our_height);

        if start <= our_height {
            for h in start..=effective_end {
                if let Ok(block) = context.blockchain.get_block_by_height(h).await {
                    blocks.push(block);
                }
            }
        }

        info!(
            "📤 [{}] Sending {} blocks to {} (requested {}-{}, effective {}-{})",
            self.direction,
            blocks.len(),
            self.peer_ip,
            start,
            end,
            start,
            effective_end
        );

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
                    tier: mn_info.masternode.tier.clone(),
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
        info!(
            "📦 [{}] Received TSDC block proposal at height {} from {}",
            self.direction, block.header.height, self.peer_ip
        );

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
                Err(e) => {
                    warn!(
                        "[{}] Failed to broadcast prepare vote: {}",
                        self.direction, e
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

                let _ = broadcast_tx.send(precommit_vote);
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
                if let Some((_, block)) = cache.remove(&block_hash) {
                    // 2. Collect precommit signatures (TODO: implement signature collection)
                    let _signatures: Vec<Vec<u8>> = vec![]; // TODO: Collect actual signatures

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
                    // Calculate reward
                    let height = block.header.height;
                    let ln_height = if height == 0 {
                        0.0
                    } else {
                        (height as f64).ln()
                    };
                    let block_subsidy = (100_000_000.0 * (1.0 + ln_height)) as u64;
                    let tx_fees: u64 = block.transactions.iter().map(|tx| tx.fee_amount()).sum();
                    let total_reward = block_subsidy + tx_fees;

                    info!(
                        "💰 [{}] Block {} rewards - subsidy: {}, fees: {}, total: {:.2} TIME",
                        self.direction,
                        height,
                        block_subsidy / 100_000_000,
                        tx_fees / 100_000_000,
                        total_reward as f64 / 100_000_000.0
                    );

                    // Add block to blockchain
                    if let Err(e) = context.blockchain.add_block(block).await {
                        warn!(
                            "[{}] Failed to add finalized block to blockchain: {}",
                            self.direction, e
                        );
                    } else {
                        info!(
                            "✅ [{}] Added finalized block {} to blockchain",
                            self.direction,
                            hex::encode(block_hash)
                        );
                    }
                } else {
                    warn!(
                        "⚠️  [{}] Block {} not found in cache for finalization",
                        self.direction,
                        hex::encode(block_hash)
                    );
                }
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
