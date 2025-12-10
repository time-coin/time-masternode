use crate::block::types::{Block, BlockHeader};
use crate::types::{Hash256, Masternode, Transaction, TxOutput};
use chrono::Timelike;
use sha2::{Digest, Sha256};

pub struct DeterministicBlockGenerator;

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

        let total_fees: u64 = txs_sorted
            .iter()
            .map(|tx| {
                let input_sum: u64 = 0;
                let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
                input_sum.saturating_sub(output_sum)
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
}
