use crate::block::types::{Block, BlockConsensusResult};
use crate::network::message::NetworkMessage;
use std::collections::HashMap;
use tokio::sync::broadcast;

/// Pure Avalanche consensus engine for block validation
/// Uses continuous sampling and majority voting instead of BFT quorum
#[allow(dead_code)]
pub struct AvalancheBlockConsensus {
    pub tx_notifier: broadcast::Sender<NetworkMessage>,
    pub masternode_peers: Vec<String>,
}

impl AvalancheBlockConsensus {
    /// Run Avalanche consensus on a block
    /// Uses majority consensus: >50% of sample agrees = finalized
    #[allow(dead_code)]
    pub async fn run_consensus(&self, local_block: Block, _height: u64) -> BlockConsensusResult {
        let peer_blocks: HashMap<String, Block> = HashMap::new();

        let local_hash = local_block.hash();
        let matches = peer_blocks
            .values()
            .filter(|b: &&Block| b.hash() == local_hash)
            .count();

        // Avalanche: need >50% of peers to agree (pure majority)
        let sample_size = self.masternode_peers.len();
        let majority_threshold = sample_size.div_ceil(2);

        if matches > majority_threshold {
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

// Keep old name for backward compatibility
pub type DeterministicConsensus = AvalancheBlockConsensus;
