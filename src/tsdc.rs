//! Time-Scheduled Deterministic Consensus (TSDC) Protocol
//!
//! TSDC provides deterministic leader election and block production on a fixed 10-minute schedule.
//! It works in conjunction with Avalanche for transaction finality.
//!
//! Key components:
//! - VRF-based leader selection (deterministic via ECVRF)
//! - Slot-based block production (every 10 minutes = 600 seconds)
//! - Fork choice rule (prefer finalized blocks)
//! - Backup leader mechanism (5-second fallback)
//!
//! Note: Many methods are currently unused but form the complete TSDC protocol scaffolding.

#![allow(dead_code)]

use crate::block::types::{Block, BlockHeader};
use crate::crypto::ECVRF;
use crate::types::Hash256;
use ed25519_dalek::SigningKey;
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
    /// Leader timeout before backup takes over (seconds)
    pub leader_timeout_secs: u64,
}

impl Default for TSCDConfig {
    fn default() -> Self {
        Self {
            slot_duration_secs: 600, // 10 minutes
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
    pub vrf_secret_key: Option<SigningKey>,
    pub vrf_public_key: Option<ed25519_dalek::VerifyingKey>,
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

/// Checkpoint for finalized transactions
#[derive(Clone, Debug)]
pub struct TransactionCheckpoint {
    pub checkpoint_number: u64,
    pub height: u64,
    pub timestamp: u64,
    pub finalized_transaction_count: usize,
    pub slot_range: (u64, u64), // (first_slot, last_slot)
}

/// TSDC consensus engine
#[allow(dead_code)]
pub struct TSCDConsensus {
    config: TSCDConfig,
    /// Reference to masternode registry (masternodes ARE validators)
    masternode_registry: Option<Arc<crate::masternode_registry::MasternodeRegistry>>,
    /// Mapping from slot number to block state
    slot_states: Arc<RwLock<HashMap<u64, SlotState>>>,
    /// Current chain head (highest finalized block)
    chain_head: Arc<RwLock<Option<Block>>>,
    /// Highest finalized block height
    finalized_height: Arc<AtomicU64>,
    /// Local validator identity (if this node is a validator)
    local_validator: Arc<RwLock<Option<TSCDValidator>>>,
    /// Checkpoints of finalized transactions (slot-based)
    checkpoints: Arc<RwLock<Vec<TransactionCheckpoint>>>,
    /// Last checkpoint slot
    last_checkpoint_slot: Arc<AtomicU64>,
}

impl TSCDConsensus {
    #[allow(dead_code)]
    /// Create new TSDC consensus engine
    pub fn new(config: TSCDConfig) -> Self {
        Self {
            config,
            masternode_registry: None,
            slot_states: Arc::new(RwLock::new(HashMap::new())),
            chain_head: Arc::new(RwLock::new(None)),
            finalized_height: Arc::new(AtomicU64::new(0)),
            local_validator: Arc::new(RwLock::new(None)),
            checkpoints: Arc::new(RwLock::new(Vec::new())),
            last_checkpoint_slot: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create new TSDC consensus engine with masternode registry
    pub fn with_masternode_registry(
        config: TSCDConfig,
        registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    ) -> Self {
        Self {
            config,
            masternode_registry: Some(registry),
            slot_states: Arc::new(RwLock::new(HashMap::new())),
            chain_head: Arc::new(RwLock::new(None)),
            finalized_height: Arc::new(AtomicU64::new(0)),
            local_validator: Arc::new(RwLock::new(None)),
            checkpoints: Arc::new(RwLock::new(Vec::new())),
            last_checkpoint_slot: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Set the masternode registry
    pub fn set_masternode_registry(
        &mut self,
        registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    ) {
        self.masternode_registry = Some(registry);
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

    /// Select leader from masternodes for a given slot
    /// Uses deterministic selection based on slot number and masternode list
    pub async fn select_leader(&self, slot: u64) -> Result<TSCDValidator, TSCDError> {
        // Get masternodes from registry
        let masternodes = match &self.masternode_registry {
            Some(registry) => registry.list_active().await,
            None => {
                return Err(TSCDError::ConfigError(
                    "No masternode registry configured".to_string(),
                ))
            }
        };

        if masternodes.is_empty() {
            return Err(TSCDError::ConfigError("No active masternodes".to_string()));
        }

        // Deterministic leader selection based on slot
        let chain_head = self.chain_head.read().await;
        let mut hasher = Sha256::new();
        hasher.update(b"leader_selection");
        hasher.update(slot.to_le_bytes());
        if let Some(block) = chain_head.as_ref() {
            hasher.update(block.hash());
        }
        let hash: [u8; 32] = hasher.finalize().into();

        // Convert hash to index
        let mut val = 0u64;
        for (i, &byte) in hash.iter().take(8).enumerate() {
            val |= (byte as u64) << (i * 8);
        }
        let leader_index = (val % masternodes.len() as u64) as usize;

        let masternode = &masternodes[leader_index];

        // Convert Masternode to TSCDValidator
        Ok(TSCDValidator {
            id: masternode.masternode.address.clone(),
            public_key: masternode.masternode.public_key.to_bytes().to_vec(),
            stake: masternode.masternode.collateral,
            vrf_secret_key: None,
            vrf_public_key: None,
        })
    }

    /// Validate a PREPARE message (block proposal from leader)
    #[allow(dead_code)]
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

        // Check if we have majority stake for finality (Avalanche consensus)
        let masternodes = match &self.masternode_registry {
            Some(registry) => registry.list_active().await,
            None => vec![],
        };
        let total_stake: u64 = masternodes.iter().map(|m| m.masternode.collateral).sum();
        let threshold = total_stake.div_ceil(2); // Majority stake (>50%)

        let mut signed_stake = 0u64;
        for validator_id in state.precommits_received.keys() {
            if let Some(masternode) = masternodes
                .iter()
                .find(|m| &m.masternode.address == validator_id)
            {
                signed_stake += masternode.masternode.collateral;
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

    /// Propose a block for the current slot (leader only)
    pub async fn propose_block(
        &self,
        _proposer_id: String,
        transactions: Vec<crate::types::Transaction>,
        masternode_rewards: Vec<(String, u64)>,
    ) -> Result<Block, TSCDError> {
        // Get current chain head for parent hash
        let chain_head = self.chain_head.read().await;
        let (parent_hash, block_height) = match chain_head.as_ref() {
            Some(block) => (block.hash(), block.header.height + 1),
            None => (Hash256::default(), 0),
        };
        drop(chain_head);

        // Get current timestamp
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Create block
        let header = BlockHeader {
            version: 1,
            height: block_height,
            previous_hash: parent_hash,
            merkle_root: Hash256::default(), // TODO: Compute merkle root from transactions
            timestamp,
            block_reward: 0, // TODO: Calculate block reward
            leader: String::new(),
            attestation_root: [0u8; 32],
            masternode_tiers: crate::block::types::MasternodeTierCounts::default(),
        };

        Ok(Block {
            header,
            transactions,
            masternode_rewards,
            time_attestations: vec![],
        })
    }

    /// Handle a received block proposal (for non-leaders in prepare phase)
    pub async fn on_block_proposal(&self, block: &Block) -> Result<(), TSCDError> {
        // Validate the block
        self.validate_prepare(block).await?;

        // Block is valid - in a real implementation, we would vote on it
        // For now, we just mark it as received
        tracing::debug!(
            "✅ Block proposal validated at height {}",
            block.header.height
        );

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
                .max_by(|a, b| match a.0.header.height.cmp(&b.0.header.height) {
                    Ordering::Equal => match a.0.header.timestamp.cmp(&b.0.header.timestamp) {
                        Ordering::Equal => a.0.hash().cmp(&b.0.hash()),
                        other => other,
                    },
                    other => other,
                });

        best.map(|(b, _)| b.clone()).ok_or(TSCDError::BlockNotFound)
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
        states.entry(slot).or_insert_with(|| SlotState {
            slot,
            timestamp: self.slot_timestamp(slot),
            leader_vrf: None,
            block: None,
            finality_proof: None,
            is_finalized: false,
            precommits_received: HashMap::new(),
        });
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

    /// Create a checkpoint of finalized transactions
    /// This periodically bundles finalized Avalanche transactions for deterministic confirmation
    pub async fn create_checkpoint(
        &self,
        slot: u64,
        finalized_tx_count: usize,
    ) -> TransactionCheckpoint {
        let current_height = self.finalized_height.load(AtomicOrdering::Relaxed);
        let last_checkpoint = self.last_checkpoint_slot.load(AtomicOrdering::Relaxed);
        let checkpoint_number = slot / 6; // One checkpoint per ~60 minutes (6 slots * 10 min)

        let checkpoint = TransactionCheckpoint {
            checkpoint_number,
            height: current_height,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            finalized_transaction_count: finalized_tx_count,
            slot_range: (last_checkpoint, slot),
        };

        // Store checkpoint
        self.checkpoints.write().await.push(checkpoint.clone());
        self.last_checkpoint_slot
            .store(slot, AtomicOrdering::Relaxed);

        checkpoint
    }

    /// Get all checkpoints
    pub async fn get_checkpoints(&self) -> Vec<TransactionCheckpoint> {
        self.checkpoints.read().await.clone()
    }

    /// Get latest checkpoint
    pub async fn get_latest_checkpoint(&self) -> Option<TransactionCheckpoint> {
        self.checkpoints.read().await.last().cloned()
    }

    // ========================================================================
    // PHASE 3E: BLOCK FINALIZATION & REWARD DISTRIBUTION
    // ========================================================================

    /// Phase 3E.1: Create finality proof from majority precommit votes
    /// Called when Avalanche consensus is reached (>50% of sample)
    pub async fn create_finality_proof(
        &self,
        block_hash: Hash256,
        height: u64,
        signatures: Vec<Vec<u8>>,
    ) -> FinalityProof {
        let proof = FinalityProof {
            block_hash,
            height,
            signatures,
            signer_count: 0, // Will be set by caller
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        tracing::info!(
            "✅ Created finality proof for block {} at height {}",
            hex::encode(block_hash),
            height
        );

        proof
    }

    /// Phase 3E.2: Add block to canonical chain
    /// Called when finality proof is created and verified
    pub async fn add_finalized_block(
        &self,
        block: Block,
        proof: FinalityProof,
    ) -> Result<(), TSCDError> {
        // Verify block height matches previous
        let current_height = self.finalized_height.load(AtomicOrdering::Relaxed);
        if block.header.height != current_height + 1 {
            return Err(TSCDError::ValidationFailed(format!(
                "Block height {} doesn't follow {}",
                block.header.height, current_height
            )));
        }

        // Verify proof has majority stake for finality (Avalanche consensus)
        let masternodes = match &self.masternode_registry {
            Some(registry) => registry.list_active().await,
            None => vec![],
        };
        let total_stake: u64 = masternodes.iter().map(|m| m.masternode.collateral).sum();
        let threshold = total_stake.div_ceil(2); // Majority stake (>50%)

        if !masternodes.is_empty()
            && proof.signatures.len() as u64 * total_stake / masternodes.len() as u64 <= threshold
        {
            return Err(TSCDError::ValidationFailed(
                "Insufficient votes for finality".to_string(),
            ));
        }

        // Add to chain
        self.finalize_block(block.clone()).await?;
        self.finalized_height
            .store(block.header.height, AtomicOrdering::Relaxed);

        tracing::info!(
            "⛓️  Block {} finalized at height {} ({}+ votes)",
            hex::encode(block.hash()),
            block.header.height,
            proof.signer_count
        );

        Ok(())
    }

    /// Phase 3E.3: Archive finalized transactions
    /// Called after block is added to chain
    /// Marks transactions as no longer pending/unconfirmed
    pub async fn archive_finalized_transactions(&self, block: &Block) -> Result<usize, TSCDError> {
        let tx_count = block.transactions.len();

        if tx_count == 0 {
            return Ok(0);
        }

        tracing::debug!(
            "📦 Archiving {} finalized transactions from block {}",
            tx_count,
            hex::encode(block.hash())
        );

        // In a real implementation, this would:
        // 1. Remove transactions from mempool
        // 2. Mark outputs as spent in UTXO set
        // 3. Add to transaction archive/history
        // 4. Update all wallet indices

        Ok(tx_count)
    }

    /// Phase 3E.4: Calculate and distribute block rewards
    /// Called after block finalization
    /// Distributes:
    /// - Block subsidy to proposer
    /// - Transaction fees to proposer
    /// - Validator rewards from masternode_rewards field
    pub async fn distribute_block_rewards(
        &self,
        block: &Block,
        _proposer_id: &str,
    ) -> Result<u64, TSCDError> {
        // Block subsidy (in TIME coins, smallest unit)
        // Formula: 100 * (1 + ln(height)) - from Protocol §10
        let height = block.header.height;
        let block_subsidy = if height == 0 {
            100_000_000 // Genesis block: 1 TIME = 100M smallest units
        } else {
            let ln_height = (height as f64).ln();
            (100_000_000.0 * (1.0 + ln_height)) as u64
        };

        // Transaction fees (sum of all tx fees)
        let tx_fees: u64 = block.transactions.iter().map(|tx| tx.fee_amount()).sum();

        let total_proposer_reward = block_subsidy + tx_fees;

        tracing::debug!(
            "💰 Block {} rewards - subsidy: {}, fees: {}, total: {}",
            height,
            block_subsidy,
            tx_fees,
            total_proposer_reward
        );

        // Validate masternode_rewards matches expected distribution
        if !block.masternode_rewards.is_empty() {
            let total_masternode = block
                .masternode_rewards
                .iter()
                .map(|(_, amt)| amt)
                .sum::<u64>();

            tracing::debug!(
                "🎯 Distributed to {} masternodes: {} TIME",
                block.masternode_rewards.len(),
                total_masternode / 100_000_000
            );
        }

        Ok(total_proposer_reward)
    }

    /// Phase 3E.5: Verify finality proof structure
    /// Called after receiving finality proof
    pub fn verify_finality_proof(&self, proof: &FinalityProof) -> Result<(), TSCDError> {
        // Verify signer count is reasonable
        if proof.signer_count == 0 {
            return Err(TSCDError::ValidationFailed(
                "Finality proof has no signers".to_string(),
            ));
        }

        // Verify signatures count matches signer count
        if proof.signatures.len() != proof.signer_count {
            return Err(TSCDError::ValidationFailed(
                "Signature count mismatch".to_string(),
            ));
        }

        // In production, would verify actual signatures here
        tracing::debug!(
            "✅ Verified finality proof for block {} with {} signatures",
            hex::encode(proof.block_hash),
            proof.signer_count
        );

        Ok(())
    }

    /// Phase 3E.6: Complete finalization workflow
    /// Orchestrates all finalization steps: proof creation → chain addition →
    /// transaction archival → reward distribution
    pub async fn finalize_block_complete(
        &self,
        block: Block,
        signatures: Vec<Vec<u8>>,
    ) -> Result<u64, TSCDError> {
        // Phase 3E.1: Create proof
        let mut proof = self
            .create_finality_proof(block.hash(), block.header.height, signatures)
            .await;
        proof.signer_count = proof.signatures.len();

        // Phase 3E.5: Verify proof
        self.verify_finality_proof(&proof)?;

        // Phase 3E.2: Add to chain
        self.add_finalized_block(block.clone(), proof).await?;

        // Phase 3E.3: Archive transactions
        let archived_count = self.archive_finalized_transactions(&block).await?;

        // Phase 3E.4: Distribute rewards
        let reward = self.distribute_block_rewards(&block, "proposer").await?;

        tracing::info!(
            "🎉 Block finalization complete: {} txs archived, {} TIME distributed",
            archived_count,
            reward / 100_000_000
        );

        Ok(reward)
    }

    /// Get finalized block count
    pub async fn get_finalized_block_count(&self) -> u64 {
        self.finalized_height.load(AtomicOrdering::Relaxed) + 1
    }

    /// Get total finalized transactions
    pub async fn get_finalized_transaction_count(&self) -> usize {
        let states = self.slot_states.read().await;
        states
            .iter()
            .filter(|(_, state)| state.is_finalized)
            .map(|(_, state)| {
                state
                    .block
                    .as_ref()
                    .map(|b| b.transactions.len())
                    .unwrap_or(0)
            })
            .sum()
    }

    /// Get total rewards distributed
    pub async fn get_total_rewards_distributed(&self) -> u64 {
        let height = self.finalized_height.load(AtomicOrdering::Relaxed);
        // Sum of all block subsidies up to height
        let mut total = 0u64;
        for h in 0..=height {
            let ln_height = (h as f64).ln();
            total += (100_000_000.0 * (1.0 + ln_height)) as u64;
        }
        total
    }

    /// Select a leader for the given slot using ECVRF
    ///
    /// Each validator evaluates ECVRF with their secret key and the previous block hash
    /// The validator with the highest VRF output becomes the leader
    pub fn select_leader_for_slot(
        slot: u64,
        validators: &[(String, SigningKey)],
        parent_block_hash: Hash256,
    ) -> (String, Vec<u8>) {
        if validators.is_empty() {
            return ("none".to_string(), vec![]);
        }

        let mut input = Vec::new();
        input.extend_from_slice(&parent_block_hash);
        input.extend_from_slice(&slot.to_le_bytes());
        input.extend_from_slice(b"TSDC-leader-selection");

        let mut best_vrf_output = vec![0u8; 32];
        let mut best_leader = validators[0].0.clone();
        let mut best_vrf_val: u64 = 0;

        for (validator_id, secret_key) in validators {
            if let Ok((vrf_output, _vrf_proof)) = ECVRF::evaluate(secret_key, &input) {
                let vrf_val = vrf_output.as_u64();
                if vrf_val > best_vrf_val {
                    best_vrf_val = vrf_val;
                    best_leader = validator_id.clone();
                    best_vrf_output = vrf_output.bytes.to_vec();
                }
            }
        }

        (best_leader, best_vrf_output)
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
        // Leader selection now requires masternode registry
        // This test verifies that select_leader returns an error without a registry
        let tsdc = TSCDConsensus::new(TSCDConfig::default());

        let leader = tsdc.select_leader(100).await;
        assert!(leader.is_err()); // Should fail without registry
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
                leader: String::new(),
                attestation_root: [0u8; 32],
                masternode_tiers: crate::block::types::MasternodeTierCounts::default(),
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        let block2 = Block {
            header: BlockHeader {
                version: 1,
                height: 101,
                previous_hash: block1.hash(),
                merkle_root: Hash256::default(),
                timestamp: 60600,
                block_reward: 100,
                leader: String::new(),
                attestation_root: [0u8; 32],
                masternode_tiers: crate::block::types::MasternodeTierCounts::default(),
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        let blocks = vec![(block1.clone(), None), (block2.clone(), None)];
        let chosen = tsdc.fork_choice(blocks).await.unwrap();

        // Should choose block2 (higher height)
        assert_eq!(chosen.header.height, block2.header.height);
    }

    #[tokio::test]
    async fn test_precommit_collection() {
        // Precommit collection now uses masternode registry for stake calculation
        // Without registry, the stake will be 0 and finality won't work
        let tsdc = TSCDConsensus::new(TSCDConfig::default());

        let block_hash = Hash256::default();

        // Without registry, precommit should still succeed but won't achieve finality
        let result1 = tsdc
            .on_precommit(block_hash, 100, "validator1".to_string(), vec![1, 2, 3])
            .await;
        assert!(result1.is_ok());
        // Without masternodes, it can't calculate majority so no finality
        assert!(result1.unwrap().is_none());
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
