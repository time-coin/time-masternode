use crate::block::types::{Block, BlockHeader};
use crate::types::{Hash256, Masternode, Transaction, TxOutput};
use chrono::Timelike;
use sha2::{Digest, Sha256};

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
        masternodes_sorted.sort_by(|a, b| a.address.cmp(&b.address));

        let mut txs_sorted = final_transactions;
        txs_sorted.sort_by_key(|a| a.txid());

        // Calculate total fees collected from transactions
        // Fees = inputs - outputs (already validated during transaction processing)
        let total_fees: u64 = txs_sorted
            .iter()
            .filter(|_tx| !_tx.inputs.is_empty()) // Skip coinbase
            .map(|_tx| {
                // Note: Input values would need to be looked up from UTXO set
                // For now, fee is implicit in transaction (inputs - outputs)
                // This will be calculated during validation
                0u64 // Placeholder - actual fee tracking needs UTXO lookups
            })
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
        let mut masternode_rewards = Vec::new();
        let mut coinbase_outputs = Vec::new();

        if total_weight > 0 && !masternodes_sorted.is_empty() {
            for mn in &masternodes_sorted {
                let weight = mn.tier.reward_weight();
                let reward = (masternode_pool * weight) / total_weight;
                if reward > 0 {
                    masternode_rewards.push((mn.wallet_address.clone(), reward));
                    coinbase_outputs.push(TxOutput {
                        value: reward,
                        script_pubkey: mn.wallet_address.as_bytes().to_vec(),
                    });
                }
            }
        }

        let coinbase_tx = Transaction {
            version: 1,
            inputs: vec![],
            outputs: coinbase_outputs,
            lock_time: 0,
            timestamp,
        };

        let mut all_txs = vec![coinbase_tx];
        all_txs.extend(txs_sorted);

        let merkle_root = Self::merkle_root(&all_txs);

        let header = BlockHeader {
            version: 2,
            height,
            previous_hash,
            merkle_root,
            timestamp,
            block_reward: total_reward,
        };

        Block {
            header,
            transactions: all_txs,
            masternode_rewards,
            vdf_proof: crate::vdf::VDFProof {
                output: vec![0u8; 32], // Will be computed by VDF prover
                iterations: 0,
                checkpoints: vec![],
            },
        }
    }

    fn merkle_root(txs: &[Transaction]) -> Hash256 {
        if txs.is_empty() {
            return [0u8; 32];
        }
        let mut hashes: Vec<Hash256> = txs.iter().map(|tx| tx.txid()).collect();
        while hashes.len() > 1 {
            if hashes.len() % 2 == 1 {
                hashes.push(*hashes.last().unwrap());
            }
            hashes = hashes
                .chunks(2)
                .map(|pair| {
                    let mut hasher = Sha256::new();
                    hasher.update(pair[0]);
                    hasher.update(pair[1]);
                    hasher.finalize().into()
                })
                .collect();
        }
        hashes[0]
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

    /// Calculate expected block height based on genesis time
    pub fn calculate_expected_height() -> u64 {
        const GENESIS_TIMESTAMP: i64 = 1764547200; // 2025-12-01T00:00:00Z
        const BLOCK_TIME_SECONDS: i64 = 600; // 10 minutes

        let now = chrono::Utc::now().timestamp();
        let elapsed = (now - GENESIS_TIMESTAMP).max(0);
        (elapsed / BLOCK_TIME_SECONDS) as u64
    }

    /// Validate block timestamp (not in future, aligned to 10-minute intervals)
    pub fn validate_block_time(timestamp: i64) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp();

        // Reject blocks more than 30 seconds in the future
        if timestamp > now + 30 {
            return Err(format!(
                "Block timestamp {} is too far in the future (now: {})",
                timestamp, now
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
