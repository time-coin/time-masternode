use crate::block::types::{Block, BlockConsensusResult};
use crate::network::message::NetworkMessage;
use std::collections::HashMap;
use tokio::sync::broadcast;

#[allow(dead_code)]
pub struct DeterministicConsensus {
    pub tx_notifier: broadcast::Sender<NetworkMessage>,
    pub masternode_peers: Vec<String>,
}

impl DeterministicConsensus {
    #[allow(dead_code)]
    pub async fn run_consensus(&self, local_block: Block, _height: u64) -> BlockConsensusResult {
        let peer_blocks: HashMap<String, Block> = HashMap::new();

        let local_hash = local_block.hash();
        let matches = peer_blocks
            .values()
            .filter(|b: &&Block| b.hash() == local_hash)
            .count();
        let quorum = (2 * self.masternode_peers.len()).div_ceil(3);

        if matches >= quorum {
            let _ = self
                .tx_notifier
                .send(NetworkMessage::BlockAnnouncement(local_block.clone()));
            BlockConsensusResult {
                success: true,
                local_block,
                matched_peers: matches,
                total_peers: self.masternode_peers.len(),
                reconciled: None,
            }
        } else {
            BlockConsensusResult {
                success: false,
                local_block,
                matched_peers: matches,
                total_peers: self.masternode_peers.len(),
                reconciled: None,
            }
        }
    }
}
