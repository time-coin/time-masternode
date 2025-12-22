/// BFT Consensus for Block Generation
///
/// This implements a simplified Byzantine Fault Tolerant consensus protocol
/// for block generation with the following properties:
///
/// 1. **Leader Selection**: Deterministic leader selected based on block height
/// 2. **Block Proposal**: Leader proposes a block
/// 3. **Voting Phase**: All masternodes vote on the proposal
/// 4. **Commit Phase**: Block is committed if 2/3+ votes received
/// 5. **Timeout & Fallback**: If leader fails, any node can propose after timeout
///
/// Consensus Flow:
/// ```
/// 1. Leader Selection (deterministic based on height)
///    └─> Leader = hash(height + masternodes) % masternode_count
///
/// 2. Block Proposal (Leader only)
///    └─> Broadcast BlockProposal{block, signature}
///
/// 3. Voting Phase (All masternodes)
///    ├─> Validate block (transactions, previous hash, signatures)
///    ├─> Sign vote (approve/reject)
///    └─> Broadcast BlockVote{block_hash, approve, signature}
///
/// 4. Vote Collection (All nodes)
///    ├─> Collect votes for block_hash
///    ├─> Check 2/3+ threshold
///    └─> If reached → commit block
///
/// 5. Commit Phase
///    └─> Broadcast BlockCommit{block_hash, signatures[]}
///
/// 6. Timeout & Failover
///    ├─> If no proposal in 30s → emergency mode
///    └─> Any masternode can propose (first valid proposal wins)
/// ```
use crate::block::types::Block;
use crate::blockchain::Blockchain;
use crate::masternode_registry::MasternodeInfo;
use crate::network::message::NetworkMessage;
use crate::types::Hash256;
use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use ed25519_dalek::{Signer, SigningKey};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

// ===== CONSENSUS TIMEOUT CONSTANTS =====
const CONSENSUS_ROUND_TIMEOUT_SECS: u64 = 30;
#[allow(dead_code)]
const VOTE_COLLECTION_TIMEOUT_SECS: u64 = 30;
#[allow(dead_code)]
const COMMIT_TIMEOUT_SECS: u64 = 10;
#[allow(dead_code)]
const VIEW_CHANGE_TIMEOUT_SECS: u64 = 60;

/// Consensus phase tracking for proper protocol execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsensusPhase {
    PrePrepare,
    #[allow(dead_code)]
    Prepare,
    #[allow(dead_code)]
    Commit,
    Finalized,
}

/// Vote type tracking for BFT phases
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteType {
    #[allow(dead_code)]
    Prepare,
    Commit,
}

/// BFT consensus state for a specific height
#[derive(Debug, Clone)]
pub struct ConsensusRound {
    #[allow(dead_code)]
    pub height: u64,
    pub round: u64,
    pub leader: Option<String>,
    pub phase: ConsensusPhase,
    pub proposed_block: Option<Block>,
    pub votes: std::collections::HashMap<String, BlockVote>,
    pub start_time: Instant,
    #[allow(dead_code)]
    pub timeout_at: Instant,
    pub finalized_block: Option<Block>,
}

#[derive(Debug, Clone)]
pub struct BlockVote {
    pub block_hash: Hash256,
    pub voter: String,
    pub vote_type: VoteType,
    pub approve: bool,
    #[allow(dead_code)]
    pub signature: Vec<u8>,
}

impl ConsensusRound {
    #[allow(dead_code)]
    pub fn prepare_vote_count(&self) -> usize {
        self.votes
            .values()
            .filter(|v| v.vote_type == VoteType::Prepare && v.approve)
            .count()
    }

    pub fn commit_vote_count(&self) -> usize {
        self.votes
            .values()
            .filter(|v| v.vote_type == VoteType::Commit && v.approve)
            .count()
    }
}

/// Optimized BFT Consensus using DashMap and lock-free primitives
pub struct BFTConsensus {
    // Per-height consensus rounds (lock-free DashMap)
    rounds: DashMap<u64, ConsensusRound>,

    // Block hash -> height index for O(1) vote routing
    block_hash_index: DashMap<Hash256, u64>,

    // Committed blocks (simple mutex, rarely contested)
    committed_blocks: Mutex<Vec<Block>>,

    // Set-once fields
    broadcast_callback: OnceLock<Arc<dyn Fn(NetworkMessage) + Send + Sync>>,
    signing_key: OnceLock<SigningKey>,

    // May be updated (blockchain reference)
    blockchain: ArcSwapOption<Blockchain>,

    // Immutable identity
    our_address: String,

    // Masternode count for quorum calculation
    masternode_count: AtomicUsize,
}

impl BFTConsensus {
    pub fn new(our_address: String) -> Self {
        Self {
            rounds: DashMap::new(),
            block_hash_index: DashMap::new(),
            committed_blocks: Mutex::new(Vec::new()),
            broadcast_callback: OnceLock::new(),
            signing_key: OnceLock::new(),
            blockchain: ArcSwapOption::empty(),
            our_address,
            masternode_count: AtomicUsize::new(0),
        }
    }

    /// Set the signing key (once-only)
    pub fn set_signing_key(&self, key: SigningKey) -> Result<(), String> {
        self.signing_key
            .set(key)
            .map_err(|_| "Signing key already set".to_string())
    }

    /// Set blockchain reference
    pub fn set_blockchain(&self, blockchain: Arc<Blockchain>) {
        self.blockchain.store(Some(blockchain));
    }

    /// Set broadcast callback (once-only)
    pub fn set_broadcast_callback<F>(&self, callback: F) -> Result<(), String>
    where
        F: Fn(NetworkMessage) + Send + Sync + 'static,
    {
        self.broadcast_callback
            .set(Arc::new(callback))
            .map_err(|_| "Broadcast callback already set".to_string())
    }

    /// Update masternode count
    #[allow(dead_code)]
    pub fn update_masternode_count(&self, count: usize) {
        self.masternode_count.store(count, Ordering::Relaxed);
    }

    /// Get quorum size for current masternode count
    fn get_quorum_size(&self) -> usize {
        let count = self.masternode_count.load(Ordering::Relaxed);
        Self::calculate_quorum_size(count)
    }

    /// Calculate quorum (2/3 + 1)
    fn calculate_quorum_size(masternode_count: usize) -> usize {
        (masternode_count * 2 / 3) + 1
    }

    fn broadcast(&self, msg: NetworkMessage) {
        if let Some(callback) = self.broadcast_callback.get() {
            callback(msg);
        }
    }

    /// Select deterministic leader for a given height
    pub fn select_leader(height: u64, masternodes: &[MasternodeInfo]) -> Option<String> {
        if masternodes.is_empty() {
            return None;
        }

        let mut sorted: Vec<_> = masternodes.iter().collect();
        sorted.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

        let mut seed_data = height.to_le_bytes().to_vec();
        for mn in sorted.iter() {
            seed_data.extend_from_slice(mn.masternode.address.as_bytes());
        }

        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(&seed_data);
        let index = u64::from_le_bytes(hash[0..8].try_into().unwrap()) % (masternodes.len() as u64);

        Some(sorted[index as usize].masternode.address.clone())
    }

    /// Check if we are the leader for this height
    pub fn are_we_leader(&self, height: u64, masternodes: &[MasternodeInfo]) -> bool {
        if let Some(leader) = Self::select_leader(height, masternodes) {
            leader == self.our_address
        } else {
            false
        }
    }

    /// Start a new consensus round for a height
    pub async fn start_round(&self, height: u64, masternodes: &[MasternodeInfo]) {
        let now = Instant::now();
        let timeout = now + Duration::from_secs(CONSENSUS_ROUND_TIMEOUT_SECS);
        let leader = Self::select_leader(height, masternodes);

        let round = ConsensusRound {
            height,
            round: 0,
            leader: leader.clone(),
            phase: ConsensusPhase::PrePrepare,
            proposed_block: None,
            votes: std::collections::HashMap::new(),
            start_time: now,
            timeout_at: timeout,
            finalized_block: None,
        };

        self.rounds.insert(height, round);

        if let Some(leader_addr) = leader {
            tracing::info!(
                "🏆 BFT Round started for height {}: Leader is {} (timeout in 30s)",
                height,
                if leader_addr == self.our_address {
                    "US"
                } else {
                    &leader_addr
                }
            );
        }
    }

    /// Check all rounds for timeout and handle view changes
    #[allow(dead_code)]
    pub async fn check_all_timeouts(&self) {
        let now = Instant::now();
        let mut timed_out = Vec::new();

        // Collect timed out rounds
        for entry in self.rounds.iter() {
            let round = entry.value();
            if round.phase != ConsensusPhase::Finalized && now > round.timeout_at {
                timed_out.push(*entry.key());
            }
        }

        // Handle timeouts outside iterator
        for height in timed_out {
            if let Some(mut round) = self.rounds.get_mut(&height) {
                tracing::warn!(
                    "⏱️ BFT timeout at height {} (phase: {:?})",
                    height,
                    round.phase
                );

                // View change
                round.round += 1;
                round.phase = ConsensusPhase::PrePrepare;
                round.proposed_block = None;
                round.votes.clear();
                round.timeout_at = now + Duration::from_secs(CONSENSUS_ROUND_TIMEOUT_SECS);
            }
        }
    }

    /// Start background timeout monitor
    #[allow(dead_code)]
    pub fn start_timeout_monitor(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let consensus = Arc::clone(self);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));

            loop {
                interval.tick().await;
                consensus.check_all_timeouts().await;
            }
        })
    }

    /// Propose a block for consensus
    pub async fn propose_block(&self, block: Block, signature: Vec<u8>) -> Result<(), String> {
        let height = block.header.height;
        let block_hash = block.hash();

        if let Some(mut round) = self.rounds.get_mut(&height) {
            // Only accept proposals in PrePrepare phase
            if round.phase != ConsensusPhase::PrePrepare {
                return Err(format!("Cannot propose in {:?} phase", round.phase));
            }

            round.proposed_block = Some(block.clone());
            round.phase = ConsensusPhase::Prepare;

            // Index the block hash for vote routing
            self.block_hash_index.insert(block_hash, height);

            self.broadcast(NetworkMessage::BlockProposal {
                block,
                proposer: self.our_address.clone(),
                signature,
                round: round.round,
            });

            Ok(())
        } else {
            Err(format!("No consensus round at height {}", height))
        }
    }

    /// Handle an incoming vote
    pub fn handle_vote(&self, vote: BlockVote) -> Result<(), String> {
        // O(1) lookup instead of O(n)
        let height = self
            .block_hash_index
            .get(&vote.block_hash)
            .map(|entry| *entry.value())
            .ok_or("Block not found for vote")?;

        if let Some(mut round) = self.rounds.get_mut(&height) {
            // Check for duplicate
            if round.votes.contains_key(&vote.voter) {
                return Err(format!("Duplicate vote from {}", vote.voter));
            }

            round.votes.insert(vote.voter.clone(), vote.clone());

            // Check consensus without holding lock
            let should_commit = {
                let quorum = self.get_quorum_size();
                let approve_count = round.commit_vote_count();
                approve_count >= quorum
            };

            if should_commit {
                if let Some(block) = round.proposed_block.clone() {
                    let block_hash = block.hash();
                    round.phase = ConsensusPhase::Finalized;
                    round.finalized_block = Some(block.clone());

                    drop(round);

                    self.broadcast(NetworkMessage::BlockCommit {
                        block_hash,
                        height,
                        signatures: Vec::new(),
                    });
                }
            }

            Ok(())
        } else {
            Err(format!("No consensus round at height {}", height))
        }
    }

    /// Handle incoming block proposal
    pub async fn handle_proposal(
        &self,
        block: Block,
        proposer: String,
        _signature: Vec<u8>,
        round: u64,
    ) -> Result<(), String> {
        let height = block.header.height;

        if let Some(round_entry) = self.rounds.get(&height) {
            if round_entry.round != round {
                return Err(format!(
                    "Round mismatch: expected {}, got {}",
                    round_entry.round, round
                ));
            }

            let is_leader = round_entry.leader.as_ref() == Some(&proposer);
            let is_emergency = round_entry.start_time.elapsed().as_secs() > 30;

            if !is_leader && !is_emergency {
                return Err(format!(
                    "Proposal from non-leader {} (expected {:?})",
                    proposer, round_entry.leader
                ));
            }
        } else {
            return Err(format!("No consensus round at height {}", height));
        }

        if let Some(mut round_entry) = self.rounds.get_mut(&height) {
            round_entry.proposed_block = Some(block.clone());
            self.block_hash_index.insert(block.hash(), height);
        }

        Ok(())
    }

    /// Get committed blocks
    pub fn get_committed_blocks(&self) -> Vec<Block> {
        self.committed_blocks.lock().clone()
    }

    /// Clear committed blocks
    #[allow(dead_code)]
    pub fn clear_committed_blocks(&self) {
        self.committed_blocks.lock().clear();
    }

    /// Get round info for monitoring
    #[allow(dead_code)]
    pub fn get_round_info(&self, height: u64) -> Option<(u64, ConsensusPhase, usize)> {
        self.rounds
            .get(&height)
            .map(|r| (r.round, r.phase, r.votes.len()))
    }

    /// Sign a block proposal
    pub async fn sign_block(&self, block: &Block) -> Vec<u8> {
        if let Some(signing_key) = self.signing_key.get() {
            let block_hash = block.hash();
            let signature = signing_key.sign(&block_hash);
            signature.to_bytes().to_vec()
        } else {
            tracing::warn!("No signing key available for block proposal");
            vec![0u8; 64]
        }
    }
}
