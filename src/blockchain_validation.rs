//! Block validation logic
//!
//! This module contains all block and chain validation rules,
//! extracted from blockchain.rs for better organization.

use crate::block::types::Block;
use crate::blockchain_error::{BlockchainError, BlockchainResult};
use crate::constants;

/// Block validator with configurable rules
pub struct BlockValidator {
    network_type: crate::NetworkType,
}

impl BlockValidator {
    /// Create a new block validator
    pub fn new(network_type: crate::NetworkType) -> Self {
        Self { network_type }
    }

    /// Validate a block structure and rules
    ///
    /// # Arguments
    /// * `block` - The block to validate
    /// * `expected_prev_hash` - Expected previous block hash (None for genesis)
    ///
    /// # Returns
    /// * `Ok(())` if block is valid
    /// * `Err(BlockchainError)` with specific error if invalid
    pub fn validate_block(
        &self,
        block: &Block,
        expected_prev_hash: Option<[u8; 32]>,
    ) -> BlockchainResult<()> {
        let height = block.header.height;

        // 1. Verify previous hash (unless genesis)
        if let Some(prev_hash) = expected_prev_hash {
            if block.header.previous_hash != prev_hash {
                return Err(BlockchainError::PreviousHashMismatch { height });
            }
        }

        // 2. Verify merkle root
        let computed_merkle = crate::block::types::calculate_merkle_root(&block.transactions);
        if computed_merkle != block.header.merkle_root {
            return Err(BlockchainError::MerkleRootMismatch {
                height,
                expected: block.header.merkle_root,
                actual: computed_merkle,
            });
        }

        // 3. Validate timestamp
        self.validate_timestamp(block)?;

        // 4. Check for duplicate transactions
        self.check_duplicate_transactions(block)?;

        // 5. Validate block size
        self.validate_block_size(block)?;

        Ok(())
    }

    /// Validate block timestamp
    fn validate_timestamp(&self, block: &Block) -> BlockchainResult<()> {
        let now = chrono::Utc::now().timestamp();
        let tolerance = constants::blockchain::TIMESTAMP_TOLERANCE_SECS;

        // Block cannot be too far in the future
        if block.header.timestamp > now + tolerance {
            return Err(BlockchainError::InvalidTimestamp {
                height: block.header.height,
                reason: format!(
                    "Timestamp {} is {} seconds in the future (max: {})",
                    block.header.timestamp,
                    block.header.timestamp - now,
                    tolerance
                ),
            });
        }

        // For non-genesis blocks, timestamp should be after genesis
        if block.header.height > 0 {
            let genesis_time = self.network_type.genesis_timestamp();
            if block.header.timestamp < genesis_time {
                return Err(BlockchainError::InvalidTimestamp {
                    height: block.header.height,
                    reason: format!(
                        "Timestamp {} is before genesis timestamp {}",
                        block.header.timestamp, genesis_time
                    ),
                });
            }
        }

        Ok(())
    }

    /// Check for duplicate transactions in block
    fn check_duplicate_transactions(&self, block: &Block) -> BlockchainResult<()> {
        let mut seen_txids = std::collections::HashSet::new();

        for tx in &block.transactions {
            let txid = tx.txid();
            if !seen_txids.insert(txid) {
                return Err(BlockchainError::InvalidBlock {
                    height: block.header.height,
                    reason: format!("Duplicate transaction: {:?}", hex::encode(txid)),
                });
            }
        }

        Ok(())
    }

    /// Validate block size is within limits
    fn validate_block_size(&self, block: &Block) -> BlockchainResult<()> {
        let serialized = bincode::serialize(block)?;
        let size = serialized.len();
        let max_size = constants::blockchain::MAX_BLOCK_SIZE;

        if size > max_size {
            return Err(BlockchainError::BlockTooLarge {
                height: block.header.height,
                size,
                max: max_size,
            });
        }

        Ok(())
    }

    /// Validate a sequence of blocks forms a valid chain
    pub fn validate_chain_sequence(&self, blocks: &[Block]) -> BlockchainResult<()> {
        if blocks.is_empty() {
            return Ok(());
        }

        // Validate each block in sequence
        for window in blocks.windows(2) {
            let prev_block = &window[0];
            let curr_block = &window[1];

            // Height must increment by 1
            if curr_block.header.height != prev_block.header.height + 1 {
                return Err(BlockchainError::InvalidBlock {
                    height: curr_block.header.height,
                    reason: format!(
                        "Height gap: previous {} -> current {}",
                        prev_block.header.height, curr_block.header.height
                    ),
                });
            }

            // Validate this block
            let prev_hash = prev_block.hash();
            self.validate_block(curr_block, Some(prev_hash))?;

            // Timestamp must be non-decreasing
            if curr_block.header.timestamp < prev_block.header.timestamp {
                return Err(BlockchainError::InvalidTimestamp {
                    height: curr_block.header.height,
                    reason: format!(
                        "Timestamp {} is before previous block timestamp {}",
                        curr_block.header.timestamp, prev_block.header.timestamp
                    ),
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::types::{Block, BlockHeader};

    fn create_test_block(height: u64, timestamp: i64) -> Block {
        let transactions = vec![];
        let merkle_root = crate::block::types::calculate_merkle_root(&transactions);

        Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: [0u8; 32],
                merkle_root,
                timestamp,
                block_reward: 0,
                leader: "test".to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: Default::default(),
                ..Default::default()
            },
            transactions,
            masternode_rewards: vec![],
            time_attestations: vec![],
            consensus_participants: vec![],
        }
    }

    #[test]
    fn test_valid_block() {
        let validator = BlockValidator::new(crate::NetworkType::Testnet);
        let block = create_test_block(1, chrono::Utc::now().timestamp());

        assert!(validator.validate_block(&block, Some([0u8; 32])).is_ok());
    }

    #[test]
    fn test_future_timestamp_rejected() {
        let validator = BlockValidator::new(crate::NetworkType::Testnet);
        let future_time = chrono::Utc::now().timestamp() + 2000; // > 15 min tolerance
        let block = create_test_block(1, future_time);

        let result = validator.validate_block(&block, Some([0u8; 32]));
        assert!(matches!(
            result,
            Err(BlockchainError::InvalidTimestamp { .. })
        ));
    }

    #[test]
    fn test_merkle_root_validation() {
        let validator = BlockValidator::new(crate::NetworkType::Testnet);
        let mut block = create_test_block(1, chrono::Utc::now().timestamp());

        // Corrupt merkle root
        block.header.merkle_root = [1u8; 32];

        let result = validator.validate_block(&block, Some([0u8; 32]));
        assert!(matches!(
            result,
            Err(BlockchainError::MerkleRootMismatch { .. })
        ));
    }

    #[test]
    fn test_chain_sequence_validation() {
        let validator = BlockValidator::new(crate::NetworkType::Testnet);
        let now = chrono::Utc::now().timestamp();

        let block1 = create_test_block(1, now);
        let mut block2 = create_test_block(2, now + 10);

        // Set block2's previous_hash to block1's hash
        block2.header.previous_hash = block1.hash();

        let blocks = vec![block1, block2];

        assert!(validator.validate_chain_sequence(&blocks).is_ok());
    }
}
