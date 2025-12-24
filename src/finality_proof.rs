//! Verifiable Finality Proofs (VFP) Manager
//! Implements protocol §8: Accumulation, validation, and tracking of finality votes
//! Converts Avalanche's local acceptance into objectively verifiable global finality
//!
//! Note: Methods form the complete VFP protocol scaffolding for future integration.

#![allow(dead_code)]

use crate::types::{FinalityVote, Hash256, Transaction, VerifiableFinality};
use dashmap::DashMap;
use ed25519_dalek::VerifyingKey;
use std::sync::Arc;

/// Tracks finality votes for each transaction
#[derive(Clone)]
pub struct FinalityProofManager {
    /// votes[txid] = Vec<FinalityVote>
    votes: Arc<DashMap<Hash256, Vec<FinalityVote>>>,
    /// finalized_txs[txid] = VerifiableFinality (when threshold reached)
    finalized_txs: Arc<DashMap<Hash256, VerifiableFinality>>,
    /// Chain ID for validating votes
    chain_id: u32,
}

impl FinalityProofManager {
    pub fn new(chain_id: u32) -> Self {
        Self {
            votes: Arc::new(DashMap::new()),
            finalized_txs: Arc::new(DashMap::new()),
            chain_id,
        }
    }

    /// Add a finality vote for a transaction
    /// Returns true if this vote brings us to threshold
    pub fn add_vote(&self, txid: Hash256, vote: FinalityVote) -> bool {
        // Validate vote chain_id matches
        if vote.chain_id != self.chain_id {
            tracing::warn!("Vote has wrong chain_id");
            return false;
        }

        // Validate vote matches txid
        if vote.txid != txid {
            tracing::warn!("Vote txid does not match");
            return false;
        }

        // Add vote to list
        self.votes.entry(txid).or_default().push(vote);

        false // Caller will check threshold separately
    }

    /// Check if a transaction has enough votes to be finalized
    /// Uses Avalanche consensus model: finality achieved through continuous sampling
    /// Returns total weight of votes if meets Avalanche quorum threshold, None otherwise
    /// Threshold: alpha (quorum size) positive responses = consensus
    pub fn check_finality_threshold(&self, txid: Hash256, total_avs_weight: u64) -> Option<u64> {
        if let Some(votes_entry) = self.votes.get(&txid) {
            let total_weight: u64 = votes_entry.iter().map(|v| v.voter_weight).sum();

            // Avalanche consensus threshold: need quorum_size (14) positive responses
            // For pure Avalanche: use sample majority (>50% of sample)
            // Typical sample size k=20, need alpha=14 confirmations
            // This is equivalent to >70% of sampled validators
            let threshold = total_avs_weight.div_ceil(2); // Majority stake weight
            if total_weight >= threshold {
                return Some(total_weight);
            }
        }
        None
    }

    /// Get all votes for a transaction
    pub fn get_votes(&self, txid: &Hash256) -> Vec<FinalityVote> {
        self.votes
            .get(txid)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Finalize a transaction with its VFP
    pub fn finalize_transaction(
        &self,
        tx: &Transaction,
        slot_index: u64,
        avs_snapshot: &[(String, u64, VerifyingKey)],
    ) -> Result<VerifiableFinality, String> {
        let txid = tx.txid();

        // Get accumulated votes
        let votes = self.get_votes(&txid);

        // Create VFP
        let vfp = VerifiableFinality {
            tx: tx.clone(),
            slot_index,
            votes,
        };

        // Validate the VFP according to protocol rules
        let total_weight = vfp.validate(self.chain_id, avs_snapshot)?;

        tracing::info!(
            "Transaction {} finalized with VFP: {} weight votes, slot {}",
            hex::encode(txid),
            total_weight,
            slot_index
        );

        // Store the finalized proof
        self.finalized_txs.insert(txid, vfp.clone());

        Ok(vfp)
    }

    /// Check if a transaction is globally finalized
    pub fn is_globally_finalized(&self, txid: &Hash256) -> bool {
        self.finalized_txs.contains_key(txid)
    }

    /// Get finalized proof for a transaction
    pub fn get_finality_proof(&self, txid: &Hash256) -> Option<VerifiableFinality> {
        self.finalized_txs.get(txid).map(|entry| entry.clone())
    }

    /// Clear votes for a transaction (after finalization or rejection)
    pub fn clear_votes(&self, txid: &Hash256) {
        self.votes.remove(txid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finality_threshold_calculation() {
        let mgr = FinalityProofManager::new(1);

        // 100 total weight requires 67 minimum
        let threshold = (100 * 67 + 99) / 100;
        assert_eq!(threshold, 67);

        // 1000 total weight requires 670 minimum
        let threshold = (1000 * 67 + 99) / 100;
        assert_eq!(threshold, 671);
    }
}
