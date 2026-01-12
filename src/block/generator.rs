use crate::block::types::{Block, BlockHeader};
use crate::types::{sort_masternodes_canonical, Hash256, Masternode, Transaction};
use chrono::Timelike;

pub struct DeterministicBlockGenerator;

#[allow(dead_code)]
impl DeterministicBlockGenerator {
    /// Calculate total masternode reward using logarithmic scaling
    /// Formula: BASE * ln(1 + count / SCALE)
    /// Per TIME-COIN spec: Controls inflation as network grows
    pub fn calculate_total_masternode_reward(total_nodes: u64) -> u64 {
        const TIME_UNIT: u64 = 100_000_000; // 1 TIME = 100M satoshis
        const BASE_REWARD: f64 = 2000.0; // Targets ~18-35% APY
        const SCALE_FACTOR: f64 = 50.0; // Controls growth curve

        if total_nodes == 0 {
            return 0;
        }

        let total_nodes_f64 = total_nodes as f64;
        let multiplier = (1.0 + (total_nodes_f64 / SCALE_FACTOR)).ln();
        let reward = BASE_REWARD * multiplier * (TIME_UNIT as f64);

        reward as u64
    }

    pub fn generate(
        height: u64,
        previous_hash: Hash256,
        final_transactions: Vec<Transaction>,
        transaction_fees: Vec<u64>, // Actual fees paid for each transaction
        masternodes: Vec<Masternode>,
        _base_reward: u64, // Ignored, use logarithmic calculation instead
    ) -> Block {
        // Align to 10-minute clock intervals: 0, 10, 20, 30, 40, 50 minutes
        let now = chrono::Utc::now();
        let minute = now.minute();
        let aligned_minute = (minute / 10) * 10; // Round down to nearest 10

        let timestamp = now
            .date_naive()
            .and_hms_opt(now.hour(), aligned_minute, 0)
            .unwrap()
            .and_utc()
            .timestamp();

        let mut masternodes_sorted = masternodes;
        sort_masternodes_canonical(&mut masternodes_sorted);

        // Phase 1.2: Enforce canonical transaction ordering for deterministic merkle roots
        // All transactions MUST be sorted by txid to ensure all nodes compute identical merkle roots
        // This prevents consensus failures from transaction ordering differences

        // Build fee map before moving final_transactions
        let mut fee_map: std::collections::HashMap<Hash256, u64> = std::collections::HashMap::new();
        for (i, tx) in final_transactions.iter().enumerate() {
            if i < transaction_fees.len() {
                fee_map.insert(tx.txid(), transaction_fees[i]);
            }
        }

        let mut txs_sorted = final_transactions;
        txs_sorted.sort_by_key(|a| a.txid());

        // Calculate total fees collected from transactions
        let total_fees: u64 = txs_sorted
            .iter()
            .filter(|tx| !tx.inputs.is_empty()) // Skip coinbase
            .map(|tx| *fee_map.get(&tx.txid()).unwrap_or(&0))
            .sum();

        // Calculate total reward using logarithmic scaling
        let base_reward = Self::calculate_total_masternode_reward(masternodes_sorted.len() as u64);
        let total_reward = base_reward + total_fees;

        // Calculate total weight (proportional to collateral for fair APY)
        let total_weight: u64 = masternodes_sorted
            .iter()
            .map(|mn| mn.tier.reward_weight())
            .sum();

        // 100% of block rewards go to masternodes
        // No treasury or governance allocations
        let masternode_pool = total_reward;

        // Distribute masternode rewards proportionally by weight
        // Rewards are stored in masternode_rewards field and converted to UTXOs when block is processed
        // NOTE: Fees are deducted from rewards (0.1% fee on masternode rewards)
        let mut masternode_rewards = Vec::new();

        if total_weight > 0 && !masternodes_sorted.is_empty() {
            for mn in &masternodes_sorted {
                let weight = mn.tier.reward_weight();
                let gross_reward = (masternode_pool * weight) / total_weight;

                // Deduct 0.1% fee from masternode reward
                let fee = gross_reward / 1000; // 0.1% = 1/1000
                let net_reward = gross_reward.saturating_sub(fee);

                if net_reward > 0 {
                    masternode_rewards.push((mn.wallet_address.clone(), net_reward));
                }
            }
        }

        // Coinbase transaction marker (empty outputs - rewards are in masternode_rewards)
        let coinbase_tx = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            timestamp,
        };

        let mut all_txs = vec![coinbase_tx];
        all_txs.extend(txs_sorted);

        let merkle_root = crate::block::types::calculate_merkle_root(&all_txs);

        let header = BlockHeader {
            version: 2,
            height,
            previous_hash,
            merkle_root,
            timestamp,
            block_reward: total_reward,
            leader: String::new(),
            attestation_root: [0u8; 32], // Will be set when attestations are added
            masternode_tiers: crate::block::types::MasternodeTierCounts::default(),
            ..Default::default()
        };

        Block {
            header,
            transactions: all_txs,
            masternode_rewards,
            time_attestations: vec![], // Attestations added later
        }
    }

    /// Calculate next block timestamp (clock-aligned every 10 minutes)
    pub fn next_block_time() -> i64 {
        let now = chrono::Utc::now();
        let minute = now.minute();
        let aligned_minute = ((minute / 10) + 1) * 10; // Next 10-minute mark

        if aligned_minute >= 60 {
            now.date_naive()
                .and_hms_opt(now.hour() + 1, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        } else {
            now.date_naive()
                .and_hms_opt(now.hour(), aligned_minute, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        }
    }

    /// Validate block timestamp (not in future, aligned to 10-minute intervals)
    pub fn validate_block_time(timestamp: i64) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp();

        // Allow up to 10 minutes + 2 minutes grace for deterministic block scheduling
        // Blocks are created with deterministic timestamps that may be in the future
        const MAX_FUTURE: i64 = 600 + 120; // 10 min + 2 min grace
        if timestamp > now + MAX_FUTURE {
            return Err(format!(
                "Block timestamp {} is too far in the future (now: {}, max: {})",
                timestamp,
                now,
                now + MAX_FUTURE
            ));
        }

        // Verify 10-minute alignment
        let dt = chrono::DateTime::from_timestamp(timestamp, 0)
            .ok_or_else(|| "Invalid timestamp".to_string())?;
        let minute = dt.minute();

        if minute % 10 != 0 {
            return Err(format!(
                "Block timestamp not aligned to 10-minute intervals: minute={}",
                minute
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OutPoint, TxInput, TxOutput};

    #[test]
    fn test_transaction_ordering_determinism() {
        // Phase 1.2: Verify blocks with same transactions produce same merkle root
        // regardless of input order

        let timestamp = chrono::Utc::now().timestamp();

        // Create test transactions with different content
        let tx1 = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: [1u8; 32],
                    vout: 0,
                },
                script_sig: vec![1, 2, 3],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 100,
                script_pubkey: b"addr1".to_vec(),
            }],
            lock_time: 0,
            timestamp,
        };

        let tx2 = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: [2u8; 32],
                    vout: 0,
                },
                script_sig: vec![4, 5, 6],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 200,
                script_pubkey: b"addr2".to_vec(),
            }],
            lock_time: 0,
            timestamp,
        };

        let tx3 = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: [3u8; 32],
                    vout: 0,
                },
                script_sig: vec![7, 8, 9],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 300,
                script_pubkey: b"addr3".to_vec(),
            }],
            lock_time: 0,
            timestamp,
        };

        // Create blocks with transactions in different orders
        let block1 = DeterministicBlockGenerator::generate(
            1,
            [0u8; 32],
            vec![tx1.clone(), tx2.clone(), tx3.clone()],
            vec![0, 0, 0], // No fees for test
            vec![],
            100,
        );

        let block2 = DeterministicBlockGenerator::generate(
            1,
            [0u8; 32],
            vec![tx3.clone(), tx1.clone(), tx2.clone()],
            vec![0, 0, 0], // No fees for test
            vec![],
            100,
        );

        let block3 = DeterministicBlockGenerator::generate(
            1,
            [0u8; 32],
            vec![tx2, tx3, tx1],
            vec![0, 0, 0],
            vec![],
            100,
        );

        // All blocks should have identical merkle roots
        assert_eq!(
            block1.header.merkle_root, block2.header.merkle_root,
            "Blocks with same txs in different order should have same merkle root"
        );
        assert_eq!(
            block2.header.merkle_root, block3.header.merkle_root,
            "Blocks with same txs in different order should have same merkle root"
        );
    }

    #[test]
    fn test_empty_block_merkle_root() {
        // Empty blocks (only coinbase) should have consistent merkle root
        let block1 =
            DeterministicBlockGenerator::generate(1, [0u8; 32], vec![], vec![], vec![], 100);

        let block2 =
            DeterministicBlockGenerator::generate(1, [0u8; 32], vec![], vec![], vec![], 100);

        assert_eq!(
            block1.header.merkle_root, block2.header.merkle_root,
            "Empty blocks should have identical merkle roots"
        );
    }
}
