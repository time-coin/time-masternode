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
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// BFT consensus state for a specific height
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ConsensusRound {
    pub height: u64,
    pub round: u64,
    pub leader: Option<String>,
    pub proposed_block: Option<Block>,
    pub votes: HashMap<String, BlockVote>, // masternode_address -> vote
    pub start_time: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct BlockVote {
    pub block_hash: Hash256,
    pub voter: String,
    pub approve: bool,
    pub signature: Vec<u8>,
}

type BroadcastCallback = Arc<RwLock<Option<Arc<dyn Fn(NetworkMessage) + Send + Sync>>>>;

pub struct BFTConsensus {
    /// Current consensus rounds by height
    rounds: Arc<RwLock<HashMap<u64, ConsensusRound>>>,
    /// Committed blocks waiting to be added to chain
    committed_blocks: Arc<RwLock<Vec<Block>>>,
    /// Callback to broadcast messages
    broadcast_callback: BroadcastCallback,
    /// Our masternode address
    our_address: String,
    /// Our signing key for BFT messages
    signing_key: Arc<RwLock<Option<SigningKey>>>,
    /// Blockchain reference for validation
    blockchain: Arc<RwLock<Option<Arc<Blockchain>>>>,
}

impl BFTConsensus {
    pub fn new(our_address: String) -> Self {
        Self {
            rounds: Arc::new(RwLock::new(HashMap::new())),
            committed_blocks: Arc::new(RwLock::new(Vec::new())),
            broadcast_callback: Arc::new(RwLock::new(None)),
            our_address,
            signing_key: Arc::new(RwLock::new(None)),
            blockchain: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the signing key for this node
    pub async fn set_signing_key(&self, key: SigningKey) {
        *self.signing_key.write().await = Some(key);
    }

    /// Set blockchain reference for validation
    pub async fn set_blockchain(&self, blockchain: Arc<Blockchain>) {
        *self.blockchain.write().await = Some(blockchain);
    }

    /// Set broadcast callback
    pub async fn set_broadcast_callback<F>(&self, callback: F)
    where
        F: Fn(NetworkMessage) + Send + Sync + 'static,
    {
        *self.broadcast_callback.write().await = Some(Arc::new(callback));
    }

    fn broadcast(&self, msg: NetworkMessage) {
        let callback = self.broadcast_callback.clone();
        tokio::spawn(async move {
            if let Some(cb) = callback.read().await.as_ref() {
                cb(msg);
            }
        });
    }

    /// Select deterministic leader for a given height
    /// Leader is chosen by: hash(height || masternode_addresses) % masternode_count
    pub fn select_leader(height: u64, masternodes: &[MasternodeInfo]) -> Option<String> {
        if masternodes.is_empty() {
            return None;
        }

        // Sort masternodes by address for determinism
        let mut sorted: Vec<_> = masternodes.iter().collect();
        sorted.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

        // Create deterministic seed from height and addresses
        let mut seed_data = height.to_le_bytes().to_vec();
        for mn in sorted.iter() {
            seed_data.extend_from_slice(mn.masternode.address.as_bytes());
        }

        // Hash and select
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
        let leader = Self::select_leader(height, masternodes);

        let round = ConsensusRound {
            height,
            round: 0,
            leader: leader.clone(),
            proposed_block: None,
            votes: HashMap::new(),
            start_time: std::time::Instant::now(),
        };

        self.rounds.write().await.insert(height, round);

        if let Some(leader_addr) = leader {
            tracing::info!(
                "🏆 BFT Round started for height {}: Leader is {}",
                height,
                if leader_addr == self.our_address {
                    "US"
                } else {
                    &leader_addr
                }
            );
        }
    }

    /// Propose a block (leader only)
    pub async fn propose_block(&self, block: Block, signature: Vec<u8>) {
        let height = block.header.height;

        let mut rounds = self.rounds.write().await;
        if let Some(round) = rounds.get_mut(&height) {
            round.proposed_block = Some(block.clone());

            tracing::info!(
                "📋 Proposing block at height {} with {} transactions",
                height,
                block.transactions.len()
            );

            // Broadcast proposal
            self.broadcast(NetworkMessage::BlockProposal {
                block,
                proposer: self.our_address.clone(),
                signature,
                round: round.round,
            });
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

        // Validate proposal is from the expected leader
        let mut rounds = self.rounds.write().await;
        let consensus_round = rounds
            .get_mut(&height)
            .ok_or_else(|| format!("No active round for height {}", height))?;

        if consensus_round.round != round {
            return Err(format!(
                "Round mismatch: expected {}, got {}",
                consensus_round.round, round
            ));
        }

        // Check if proposer is the leader (or if we're in emergency mode)
        let is_leader = consensus_round.leader.as_ref() == Some(&proposer);
        let is_emergency = consensus_round.start_time.elapsed().as_secs() > 30;

        if !is_leader && !is_emergency {
            return Err(format!(
                "Proposal from non-leader {} (expected {:?})",
                proposer, consensus_round.leader
            ));
        }

        // Store proposal
        consensus_round.proposed_block = Some(block.clone());

        tracing::info!(
            "📥 Received block proposal for height {} from {}",
            height,
            proposer
        );

        // Automatically vote on the proposal
        self.vote_on_proposal(height).await
    }

    /// Vote on the current proposal
    async fn vote_on_proposal(&self, height: u64) -> Result<(), String> {
        let rounds = self.rounds.read().await;
        let round = rounds
            .get(&height)
            .ok_or_else(|| format!("No active round for height {}", height))?;

        let block = round
            .proposed_block
            .as_ref()
            .ok_or_else(|| "No block proposed yet".to_string())?;

        // Validate the block
        let approve = self.validate_block(block).await;

        // Create and broadcast vote
        let block_hash = block.hash();
        let signature = self.sign_vote(&block_hash, approve).await;

        let vote = BlockVote {
            block_hash,
            voter: self.our_address.clone(),
            approve,
            signature: signature.clone(),
        };

        // Store our vote
        drop(rounds);
        self.handle_vote(vote.clone()).await?;

        // Broadcast vote
        self.broadcast(NetworkMessage::BlockVote {
            block_hash,
            height,
            voter: self.our_address.clone(),
            signature,
            approve,
        });

        tracing::info!(
            "🗳️  Voted {} on block proposal at height {}",
            if approve { "APPROVE" } else { "REJECT" },
            height
        );

        Ok(())
    }

    /// Handle incoming vote
    pub async fn handle_vote(&self, vote: BlockVote) -> Result<(), String> {
        let mut rounds = self.rounds.write().await;

        // Find the round for this block
        let height = rounds
            .iter()
            .find(|(_, round)| {
                round
                    .proposed_block
                    .as_ref()
                    .map(|b| b.hash() == vote.block_hash)
                    .unwrap_or(false)
            })
            .map(|(h, _)| *h);

        if let Some(height) = height {
            if let Some(round) = rounds.get_mut(&height) {
                // Store vote (prevent double voting)
                if round.votes.contains_key(&vote.voter) {
                    return Err(format!("Duplicate vote from {}", vote.voter));
                }

                let approve_str = if vote.approve { "APPROVE" } else { "REJECT" };
                tracing::info!(
                    "📊 Received {} vote from {} for height {}",
                    approve_str,
                    vote.voter,
                    height
                );

                round.votes.insert(vote.voter.clone(), vote);

                // Check if we reached consensus
                self.check_consensus(height, &rounds).await;
            }
        }

        Ok(())
    }

    /// Check if we have enough votes to commit
    async fn check_consensus(&self, height: u64, rounds: &HashMap<u64, ConsensusRound>) {
        if let Some(round) = rounds.get(&height) {
            if let Some(block) = &round.proposed_block {
                let approve_count = round.votes.values().filter(|v| v.approve).count();
                let total_votes = round.votes.len();

                // Need 2/3+ approval
                let quorum = (total_votes * 2).div_ceil(3);

                if approve_count >= quorum {
                    tracing::info!(
                        "✅ BFT Consensus reached for height {}: {}/{} votes (quorum: {})",
                        height,
                        approve_count,
                        total_votes,
                        quorum
                    );

                    // Collect signatures
                    let signatures: Vec<(String, Vec<u8>)> = round
                        .votes
                        .iter()
                        .filter(|(_, v)| v.approve)
                        .map(|(addr, v)| (addr.clone(), v.signature.clone()))
                        .collect();

                    // Commit block
                    self.committed_blocks.write().await.push(block.clone());

                    // Broadcast commit
                    self.broadcast(NetworkMessage::BlockCommit {
                        block_hash: block.hash(),
                        height,
                        signatures,
                    });
                }
            }
        }
    }

    /// Validate a proposed block
    async fn validate_block(&self, block: &Block) -> bool {
        // Basic validation
        if block.transactions.is_empty() {
            tracing::warn!("Block has no transactions");
            return false;
        }

        // Validate timestamp is not in future
        let now = chrono::Utc::now().timestamp();
        if block.header.timestamp > now + 30 {
            tracing::warn!("Block timestamp is too far in future");
            return false;
        }

        // Check if we have blockchain reference for deeper validation
        if let Some(blockchain) = self.blockchain.read().await.as_ref() {
            // Verify previous hash matches
            if block.header.height > 0 {
                match blockchain.get_block_hash(block.header.height - 1) {
                    Ok(prev_hash) => {
                        if prev_hash != block.header.previous_hash {
                            tracing::warn!("Previous hash mismatch");
                            return false;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to get previous hash: {}", e);
                        return false;
                    }
                }
            }

            // Verify block is for expected height
            let current_height = blockchain.get_height().await;
            if block.header.height != current_height + 1 {
                tracing::warn!(
                    "Block height {} doesn't match expected {}",
                    block.header.height,
                    current_height + 1
                );
                return false;
            }
        }

        // TODO: More validation
        // - Verify merkle root
        // - Validate all transactions
        // - Check masternode signatures
        // - Verify reward amounts

        true
    }

    /// Sign a vote with our masternode key
    async fn sign_vote(&self, block_hash: &Hash256, approve: bool) -> Vec<u8> {
        if let Some(signing_key) = self.signing_key.read().await.as_ref() {
            // Create message to sign: block_hash + approve flag
            let mut message = block_hash.to_vec();
            message.push(if approve { 1 } else { 0 });

            // Sign the message
            let signature = signing_key.sign(&message);
            signature.to_bytes().to_vec()
        } else {
            tracing::warn!("No signing key available for vote");
            vec![0u8; 64] // Placeholder
        }
    }

    /// Verify a vote signature
    #[allow(dead_code)]
    pub fn verify_vote_signature(
        block_hash: &Hash256,
        approve: bool,
        signature: &[u8],
        public_key: &VerifyingKey,
    ) -> bool {
        if signature.len() != 64 {
            return false;
        }

        // Reconstruct the signed message
        let mut message = block_hash.to_vec();
        message.push(if approve { 1 } else { 0 });

        // Parse signature
        let sig_array: &[u8; 64] = match signature.try_into() {
            Ok(arr) => arr,
            Err(_) => return false,
        };
        let sig = Signature::from_bytes(sig_array);

        // Verify
        public_key.verify(&message, &sig).is_ok()
    }

    /// Sign a block proposal
    pub async fn sign_block(&self, block: &Block) -> Vec<u8> {
        if let Some(signing_key) = self.signing_key.read().await.as_ref() {
            let block_hash = block.hash();
            let signature = signing_key.sign(&block_hash);
            signature.to_bytes().to_vec()
        } else {
            tracing::warn!("No signing key available for block proposal");
            vec![0u8; 64] // Placeholder
        }
    }

    /// Get committed blocks ready to be added to chain
    pub async fn get_committed_blocks(&self) -> Vec<Block> {
        let mut committed = self.committed_blocks.write().await;
        let blocks = committed.drain(..).collect();
        blocks
    }

    /// Check if consensus round has timed out (30 seconds)
    #[allow(dead_code)]
    pub async fn check_timeout(&self, height: u64) -> bool {
        if let Some(round) = self.rounds.read().await.get(&height) {
            round.start_time.elapsed().as_secs() > 30
        } else {
            false
        }
    }

    /// Clean up old rounds
    #[allow(dead_code)]
    pub async fn cleanup_old_rounds(&self, current_height: u64) {
        self.rounds
            .write()
            .await
            .retain(|h, _| *h >= current_height.saturating_sub(10));
    }
}
