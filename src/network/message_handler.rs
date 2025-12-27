//! Unified message handler for both inbound and outbound connections
//!
//! This module provides a single implementation of network message handling
//! that works regardless of connection direction. Previously, message handling
//! was duplicated between server.rs (inbound) and peer_connection.rs (outbound).

use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use std::sync::Arc;
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
    pub consensus: Arc<ConsensusEngine>,
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
        timestamp: i64,
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
