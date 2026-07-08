use super::PeerConnectionRegistry;
use crate::consensus::ConsensusEngine;
use crate::network::message::NetworkMessage;
use std::sync::Arc;
use tokio::sync::broadcast;

impl PeerConnectionRegistry {
    /// Set TimeLock consensus resources (called once after server initialization)
    pub async fn set_timelock_resources(
        &self,
        consensus: Arc<ConsensusEngine>,
        block_cache: Arc<crate::network::block_cache::BlockCache>,
        broadcast_tx: broadcast::Sender<NetworkMessage>,
    ) {
        *self.timelock_consensus.write().await = Some(consensus);
        *self.timelock_block_cache.write().await = Some(block_cache);
        *self.timelock_broadcast.write().await = Some(broadcast_tx);
    }

    /// Get TimeLock consensus resources for message handling
    pub async fn get_timelock_resources(
        &self,
    ) -> (
        Option<Arc<ConsensusEngine>>,
        Option<Arc<crate::network::block_cache::BlockCache>>,
        Option<broadcast::Sender<NetworkMessage>>,
    ) {
        (
            self.timelock_consensus.read().await.clone(),
            self.timelock_block_cache.read().await.clone(),
            self.timelock_broadcast
                .read()
                .await
                .as_ref()
                .map(|tx| tx.clone()),
        )
    }

    /// Set WebSocket transaction event sender for real-time wallet notifications
    pub async fn set_tx_event_sender(
        &self,
        sender: broadcast::Sender<crate::rpc::websocket::TransactionEvent>,
    ) {
        *self.ws_tx_event_sender.write().await = Some(sender);
    }

    /// Get WebSocket transaction event sender
    pub async fn get_tx_event_sender(
        &self,
    ) -> Option<broadcast::Sender<crate::rpc::websocket::TransactionEvent>> {
        self.ws_tx_event_sender.read().await.clone()
    }
}
