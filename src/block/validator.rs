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
        if !Self::is_midnight_utc(candidate.header.timestamp) {
            return Err("Timestamp must be midnight UTC".to_string());
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
    fn is_midnight_utc(ts: i64) -> bool {
        use chrono::{TimeZone, Timelike, Utc};
        let dt = Utc.timestamp_opt(ts, 0).single().unwrap();
        dt.hour() == 0 && dt.minute() == 0 && dt.second() == 0
    }
}
