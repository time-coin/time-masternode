use crate::block::types::Block;
use crate::types::Masternode;

#[allow(dead_code)]
pub struct BlockValidator;

impl BlockValidator {
    #[allow(dead_code)]
    pub fn validate_block(
        candidate: &Block,
        expected_height: u64,
        expected_prev_hash: [u8; 32],
        masternodes: &[Masternode],
        finalized_txs: &[crate::types::Transaction],
    ) -> Result<(), String> {
        if candidate.header.height != expected_height {
            return Err("Invalid block height".to_string());
        }
        if candidate.header.previous_hash != expected_prev_hash {
            return Err("Invalid previous hash".to_string());
        }
        if !Self::is_valid_block_time(candidate.header.timestamp) {
            return Err("Timestamp must be aligned to 10-minute intervals".to_string());
        }

        let regenerated = crate::block::generator::DeterministicBlockGenerator::generate(
            expected_height,
            expected_prev_hash,
            finalized_txs.to_vec(),
            masternodes.to_vec(),
            candidate.header.block_reward,
        );

        if candidate.hash() != regenerated.hash() {
            return Err("Block does not match deterministic generation".to_string());
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn is_valid_block_time(ts: i64) -> bool {
        // Block times are aligned to 10-minute (600 second) intervals
        ts % 600 == 0
    }
}
