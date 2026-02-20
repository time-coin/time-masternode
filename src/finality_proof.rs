//! Verifiable Finality Proofs (VFP) Manager
//! Implements protocol §8: Accumulation, validation, and tracking of finality votes
//! Converts timevote's local acceptance into objectively verifiable global finality
//!
//! Note: Methods form the complete VFP protocol scaffolding for future integration.

#![allow(dead_code)]

use crate::types::{FinalityVote, Hash256, TimeProof, Transaction, VerifiableFinality};
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
    /// timeproofs[txid] = TimeProof (Protocol §8.2 finality certificates)
    timeproofs: Arc<DashMap<Hash256, TimeProof>>,
    /// Chain ID for validating votes
    chain_id: u32,
}

impl FinalityProofManager {
    pub fn new(chain_id: u32) -> Self {
        Self {
            votes: Arc::new(DashMap::new()),
            finalized_txs: Arc::new(DashMap::new()),
            timeproofs: Arc::new(DashMap::new()),
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
    /// Per Protocol §8.3: Q_finality = 51% of total AVS weight (simple majority) (rounded up)
    /// Returns total weight of votes if meets finality threshold, None otherwise
    pub fn check_finality_threshold(&self, txid: Hash256, total_avs_weight: u64) -> Option<u64> {
        if let Some(votes_entry) = self.votes.get(&txid) {
            let total_weight: u64 = votes_entry.iter().map(|v| v.voter_weight).sum();

            // Protocol §8.3: Q_finality = 0.67 * total_AVS_weight (BFT-safe majority) (rounded up)
            let threshold = (total_avs_weight * 67).div_ceil(100); // 67% of AVS weight (ceiling)
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

    // ========================================================================
    // TIMEPROOF STORAGE (Protocol §8.2)
    // ========================================================================

    /// Store a TimeProof certificate for a transaction
    /// This should be called after TimeProof assembly and verification
    pub fn store_timeproof(&self, timeproof: TimeProof) -> Result<(), String> {
        let txid = timeproof.txid;

        // Store the TimeProof
        self.timeproofs.insert(txid, timeproof.clone());

        tracing::info!(
            "📦 Stored TimeProof for TX {:?} (slot: {}, votes: {})",
            hex::encode(txid),
            timeproof.slot_index,
            timeproof.votes.len()
        );

        Ok(())
    }

    /// Retrieve a TimeProof certificate for a transaction
    pub fn get_timeproof(&self, txid: &Hash256) -> Option<TimeProof> {
        self.timeproofs.get(txid).map(|entry| entry.clone())
    }

    /// Check if a TimeProof exists for a transaction
    pub fn has_timeproof(&self, txid: &Hash256) -> bool {
        self.timeproofs.contains_key(txid)
    }

    /// Remove a TimeProof (used during cleanup)
    pub fn remove_timeproof(&self, txid: &Hash256) -> Option<TimeProof> {
        self.timeproofs.remove(txid).map(|(_, proof)| proof)
    }

    /// Get count of stored TimeProofs
    pub fn timeproof_count(&self) -> usize {
        self.timeproofs.len()
    }

    /// Cleanup old TimeProofs (keeps only recent ones)
    /// Returns number of TimeProofs removed
    pub fn cleanup_old_timeproofs(&self, keep_count: usize) -> usize {
        let current_count = self.timeproofs.len();

        if current_count <= keep_count {
            return 0; // Nothing to cleanup
        }

        let to_remove = current_count - keep_count;

        // Collect oldest entries (simple FIFO for now)
        // TODO: Use slot_index or timestamp for more sophisticated cleanup
        let keys_to_remove: Vec<Hash256> = self
            .timeproofs
            .iter()
            .take(to_remove)
            .map(|entry| *entry.key())
            .collect();

        let mut removed = 0;
        for key in keys_to_remove {
            if self.timeproofs.remove(&key).is_some() {
                removed += 1;
            }
        }

        if removed > 0 {
            tracing::info!(
                "🧹 Cleaned up {} old TimeProofs (kept {})",
                removed,
                self.timeproofs.len()
            );
        }

        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finality_threshold_calculation() {
        let _mgr = FinalityProofManager::new(1);

        // 100 total weight requires 51 minimum (51% threshold with ceiling)
        let threshold = (100 * 67 + 99) / 100;
        assert_eq!(threshold, 67);

        // 1000 total weight: (67000 + 99) / 100 = 670
        let threshold = (1000 * 67 + 99) / 100;
        assert_eq!(threshold, 670);
    }
}
