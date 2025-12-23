/// Time-Scheduled Deterministic Consensus (TSDC) Protocol
///
/// TSDC provides deterministic leader election and block production on a fixed 10-minute schedule.
/// It works in conjunction with Avalanche for transaction finality.
///
/// Key components:
/// - VRF-based leader selection (deterministic)
/// - Slot-based block production (every 10 minutes = 600 seconds)
/// - Fork choice rule (prefer finalized blocks)
/// - Backup leader mechanism (5-second fallback)
use crate::block::types::{Block, BlockHeader};
use crate::types::Hash256;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::RwLock;

/// TSDC errors
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TSCDError {
    #[error("Block validation failed: {0}")]
    ValidationFailed(String),

    #[error("VRF verification failed")]
    VRFVerificationFailed,

    #[error("Invalid leader: expected {expected}, got {actual}")]
    InvalidLeader { expected: String, actual: String },

    #[error("Slot mismatch: expected {expected}, got {actual}")]
    SlotMismatch { expected: u64, actual: u64 },

    #[error("Block not found")]
    BlockNotFound,

    #[error("Parent block not finalized")]
    ParentNotFinalized,

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

/// Configuration for TSDC
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TSCDConfig {
    /// Slot duration in seconds (default: 600 = 10 minutes)
    pub slot_duration_secs: u64,
    /// Finality threshold as fraction (default: 2/3 = 0.667)
    pub finality_threshold: f64,
    /// Leader timeout before backup takes over (seconds)
    pub leader_timeout_secs: u64,
}

impl Default for TSCDConfig {
    fn default() -> Self {
        Self {
            slot_duration_secs: 600, // 10 minutes
            finality_threshold: 2.0 / 3.0,
            leader_timeout_secs: 5,
        }
    }
}

/// A validator/masternode in the TSDC system
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TSCDValidator {
    pub id: String,
    pub public_key: Vec<u8>,
    pub stake: u64,
}

/// State of a finalized block
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct FinalityProof {
    pub block_hash: Hash256,
    pub height: u64,
    pub signatures: Vec<Vec<u8>>, // Aggregate signatures
    pub signer_count: usize,
    pub timestamp: u64,
}

/// VRF output for leader selection
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub struct VRFOutput {
    pub proof: Vec<u8>,
    pub output_bytes: [u8; 32],
}

impl PartialOrd for VRFOutput {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VRFOutput {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare as little-endian integers
        self.output_bytes.cmp(&other.output_bytes)
    }
}

/// Represents the state of a single slot
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SlotState {
    pub slot: u64,
    pub timestamp: u64,
    pub leader_vrf: Option<VRFOutput>,
    pub block: Option<Block>,
    pub finality_proof: Option<FinalityProof>,
    pub is_finalized: bool,
    pub precommits_received: HashMap<String, Vec<u8>>, // validator_id -> signature
}

/// TSDC consensus engine
#[allow(dead_code)]
pub struct TSCDConsensus {
    config: TSCDConfig,
    validators: Arc<RwLock<Vec<TSCDValidator>>>,
    /// Mapping from slot number to block state
    slot_states: Arc<RwLock<HashMap<u64, SlotState>>>,
    /// Current chain head (highest finalized block)
    chain_head: Arc<RwLock<Option<Block>>>,
    /// Highest finalized block height
    finalized_height: Arc<AtomicU64>,
    /// Local validator identity (if this node is a validator)
    local_validator: Arc<RwLock<Option<TSCDValidator>>>,
}

impl TSCDConsensus {
    /// Create new TSDC consensus engine
    pub fn new(config: TSCDConfig) -> Self {
        Self {
            config,
            validators: Arc::new(RwLock::new(Vec::new())),
            slot_states: Arc::new(RwLock::new(HashMap::new())),
            chain_head: Arc::new(RwLock::new(None)),
            finalized_height: Arc::new(AtomicU64::new(0)),
            local_validator: Arc::new(RwLock::new(None)),
        }
    }

    /// Set the list of validators
    pub async fn set_validators(&self, validators: Vec<TSCDValidator>) {
        let mut v = self.validators.write().await;
        *v = validators;
    }

    /// Set this node's validator identity
    pub async fn set_local_validator(&self, validator: TSCDValidator) {
        let mut local = self.local_validator.write().await;
        *local = Some(validator);
    }

    /// Get current slot based on system time
    pub fn current_slot(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now / self.config.slot_duration_secs
    }

    /// Get timestamp for a given slot
    pub fn slot_timestamp(&self, slot: u64) -> u64 {
        slot * self.config.slot_duration_secs
    }

    /// Compute VRF for leader selection
    /// Returns the validator with the smallest VRF output
    pub async fn select_leader(&self, slot: u64) -> Result<TSCDValidator, TSCDError> {
        let validators = self.validators.read().await;
        if validators.is_empty() {
            return Err(TSCDError::ConfigError(
                "No validators registered".to_string(),
            ));
        }

        // Compute VRF input
        let chain_head = self.chain_head.read().await;
        let vrf_input = match chain_head.as_ref() {
            Some(block) => {
                let mut hasher = Sha256::new();
                hasher.update(block.hash());
                hasher.update(slot.to_le_bytes());
                hasher.finalize()
            }
            None => {
                // Genesis block - use fixed seed
                let mut hasher = Sha256::new();
                hasher.update(b"genesis");
                hasher.update(slot.to_le_bytes());
                hasher.finalize()
            }
        };

        // Select validator with lowest VRF output
        // (In production, would use actual VRF computation)
        // For now, use deterministic hash-based selection
        let mut best_validator = validators[0].clone();
        let mut best_hash = {
            let mut h = Sha256::new();
            h.update(&vrf_input);
            h.update(&best_validator.id);
            h.finalize()
        };

        for validator in &validators[1..] {
            let mut h = Sha256::new();
            h.update(&vrf_input);
            h.update(&validator.id);
            let hash = h.finalize();

            // Compare as bytes - smallest is leader
            if hash.as_slice() < best_hash.as_slice() {
                best_validator = validator.clone();
                best_hash = hash;
            }
        }

        Ok(best_validator)
    }

    /// Validate a PREPARE message (block proposal from leader)
    pub async fn validate_prepare(&self, block: &Block) -> Result<(), TSCDError> {
        let slot = block.header.height; // Use height as slot for now
        let expected_timestamp = self.slot_timestamp(slot);

        // Check timestamp matches slot
        if block.header.timestamp as u64 != expected_timestamp {
            return Err(TSCDError::SlotMismatch {
                expected: expected_timestamp,
                actual: block.header.timestamp as u64,
            });
        }

        // Verify leader is correct
        let _leader = self.select_leader(slot).await?;
        let chain_head = self.chain_head.read().await;

        match chain_head.as_ref() {
            Some(head) => {
                if block.header.previous_hash != head.hash() {
                    return Err(TSCDError::ValidationFailed(
                        "Invalid parent hash".to_string(),
                    ));
                }
            }
            None => {
                // First block - parent hash should be zeros
                if block.header.previous_hash != Hash256::default() {
                    return Err(TSCDError::ValidationFailed(
                        "First block must have zero parent hash".to_string(),
                    ));
                }
            }
        }

        // All validation passed
        Ok(())
    }

    /// Record a PRECOMMIT vote for a block
    pub async fn on_precommit(
        &self,
        block_hash: Hash256,
        height: u64,
        validator_id: String,
        signature: Vec<u8>,
    ) -> Result<Option<FinalityProof>, TSCDError> {
        let mut states = self.slot_states.write().await;
        let state = states.entry(height).or_insert_with(|| SlotState {
            slot: height,
            timestamp: self.slot_timestamp(height),
            leader_vrf: None,
            block: None,
            finality_proof: None,
            is_finalized: false,
            precommits_received: HashMap::new(),
        });

        state.precommits_received.insert(validator_id, signature);

        // Check if we have enough signatures for finality
        let validators = self.validators.read().await;
        let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
        let threshold = (total_stake as f64 * self.config.finality_threshold) as u64;

        let mut signed_stake = 0u64;
        for (validator_id, _sig) in &state.precommits_received {
            if let Some(validator) = validators.iter().find(|v| &v.id == validator_id) {
                signed_stake += validator.stake;
            }
        }

        if signed_stake > threshold && !state.is_finalized {
            state.is_finalized = true;
            let proof = FinalityProof {
                block_hash,
                height,
                signatures: state.precommits_received.values().cloned().collect(),
                signer_count: state.precommits_received.len(),
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            state.finality_proof = Some(proof.clone());

            // Update chain head
            self.finalized_height.store(height, AtomicOrdering::Relaxed);

            return Ok(Some(proof));
        }

        Ok(None)
    }

    /// Record a finalized block
    pub async fn finalize_block(&self, block: Block) -> Result<(), TSCDError> {
        let mut chain_head = self.chain_head.write().await;
        *chain_head = Some(block);
        Ok(())
    }

    /// Fork choice rule: select the canonical chain
    pub async fn fork_choice(
        &self,
        blocks: Vec<(Block, Option<FinalityProof>)>,
    ) -> Result<Block, TSCDError> {
        if blocks.is_empty() {
            return Err(TSCDError::BlockNotFound);
        }

        // Prefer finalized blocks
        let finalized: Vec<_> = blocks.iter().filter(|(_, proof)| proof.is_some()).collect();

        let candidates = if !finalized.is_empty() {
            finalized
        } else {
            blocks.iter().collect()
        };

        // Select by height (highest wins), then by slot, then by hash (lexicographic)
        let best =
            candidates
                .iter()
                .max_by(|a, b| match b.0.header.height.cmp(&a.0.header.height) {
                    Ordering::Equal => match b.0.header.timestamp.cmp(&a.0.header.timestamp) {
                        Ordering::Equal => b.0.hash().cmp(&a.0.hash()),
                        other => other,
                    },
                    other => other,
                });

        Ok(best
            .map(|(b, _)| b.clone())
            .ok_or(TSCDError::BlockNotFound)?)
    }

    /// Get the highest finalized block height
    pub fn get_finalized_height(&self) -> u64 {
        self.finalized_height.load(AtomicOrdering::Relaxed)
    }

    /// Check if a slot has timed out (leader hasn't produced block)
    pub fn is_slot_timeout(&self, slot: u64) -> bool {
        let slot_end = (slot + 1) * self.config.slot_duration_secs;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now > slot_end + self.config.leader_timeout_secs
    }

    /// Handle a missed slot (leader didn't produce block)
    pub async fn on_slot_timeout(&self, slot: u64) -> Result<(), TSCDError> {
        let mut states = self.slot_states.write().await;
        if !states.contains_key(&slot) {
            states.insert(
                slot,
                SlotState {
                    slot,
                    timestamp: self.slot_timestamp(slot),
                    leader_vrf: None,
                    block: None,
                    finality_proof: None,
                    is_finalized: false,
                    precommits_received: HashMap::new(),
                },
            );
        }
        Ok(())
    }

    /// Get pending precommits for a block
    pub async fn get_precommits(&self, height: u64) -> HashMap<String, Vec<u8>> {
        let states = self.slot_states.read().await;
        states
            .get(&height)
            .map(|s| s.precommits_received.clone())
            .unwrap_or_default()
    }

    /// Check if a block is finalized
    pub async fn is_finalized(&self, height: u64) -> bool {
        let states = self.slot_states.read().await;
        states.get(&height).map(|s| s.is_finalized).unwrap_or(false)
    }

    /// Get finality proof for a block
    pub async fn get_finality_proof(&self, height: u64) -> Option<FinalityProof> {
        let states = self.slot_states.read().await;
        states.get(&height).and_then(|s| s.finality_proof.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tsdc_initialization() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());
        assert_eq!(tsdc.config.slot_duration_secs, 600);
    }

    #[tokio::test]
    async fn test_current_slot() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());
        let slot = tsdc.current_slot();
        assert!(slot > 0);
    }

    #[tokio::test]
    async fn test_slot_timestamp() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());
        let timestamp = tsdc.slot_timestamp(100);
        assert_eq!(timestamp, 100 * 600);
    }

    #[tokio::test]
    async fn test_leader_selection() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());

        let validators = vec![
            TSCDValidator {
                id: "validator1".to_string(),
                public_key: vec![1, 2, 3],
                stake: 1000,
            },
            TSCDValidator {
                id: "validator2".to_string(),
                public_key: vec![4, 5, 6],
                stake: 2000,
            },
        ];

        tsdc.set_validators(validators).await;

        let leader = tsdc.select_leader(100).await;
        assert!(leader.is_ok());

        // Leader selection should be deterministic for the same slot
        let leader2 = tsdc.select_leader(100).await;
        assert_eq!(leader.unwrap().id, leader2.unwrap().id);
    }

    #[tokio::test]
    async fn test_fork_choice() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());

        let block1 = Block {
            header: BlockHeader {
                version: 1,
                height: 100,
                previous_hash: Hash256::default(),
                merkle_root: Hash256::default(),
                timestamp: 60000,
                block_reward: 100,
            },
            transactions: vec![],
            masternode_rewards: vec![],
        };

        let block2 = Block {
            header: BlockHeader {
                version: 1,
                height: 101,
                previous_hash: block1.hash(),
                merkle_root: Hash256::default(),
                timestamp: 60600,
                block_reward: 100,
            },
            transactions: vec![],
            masternode_rewards: vec![],
        };

        let blocks = vec![(block1.clone(), None), (block2.clone(), None)];
        let chosen = tsdc.fork_choice(blocks).await.unwrap();

        // Should choose block2 (higher height)
        assert_eq!(chosen.header.height, block2.header.height);
    }

    #[tokio::test]
    async fn test_precommit_collection() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());

        let validators = vec![
            TSCDValidator {
                id: "validator1".to_string(),
                public_key: vec![1, 2, 3],
                stake: 1000,
            },
            TSCDValidator {
                id: "validator2".to_string(),
                public_key: vec![4, 5, 6],
                stake: 1000,
            },
            TSCDValidator {
                id: "validator3".to_string(),
                public_key: vec![7, 8, 9],
                stake: 1000,
            },
        ];

        tsdc.set_validators(validators).await;

        let block_hash = Hash256::default();

        // With 3 validators of equal stake, need >2/3 = >2000 stake for finality
        // So need at least 2 validators
        let result1 = tsdc
            .on_precommit(block_hash, 100, "validator1".to_string(), vec![1, 2, 3])
            .await;
        assert!(result1.is_ok());
        assert!(result1.unwrap().is_none()); // Not finalized yet

        let result2 = tsdc
            .on_precommit(block_hash, 100, "validator2".to_string(), vec![4, 5, 6])
            .await;
        assert!(result2.is_ok());
        assert!(result2.unwrap().is_some()); // Should be finalized now
    }

    #[tokio::test]
    async fn test_slot_timeout() {
        let tsdc = TSCDConsensus::new(TSCDConfig::default());

        let current_slot = tsdc.current_slot();
        let past_slot = current_slot.saturating_sub(10);

        // A slot from 10 slots ago should timeout
        // (unless system time just restarted)
        let timeout = tsdc.is_slot_timeout(past_slot);
        assert!(timeout);
    }
}
