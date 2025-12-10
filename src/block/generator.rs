use crate::block::types::{Block, BlockHeader};
use crate::types::{Hash256, Masternode, Transaction, TxOutput};
use sha2::{Digest, Sha256};

pub struct DeterministicBlockGenerator;

impl DeterministicBlockGenerator {
    /// Calculate total masternode reward using logarithmic scaling
    /// Based on TIME Coin spec: BASE * ln(1 + count / SCALE)
    pub fn calculate_total_masternode_reward(total_nodes: u64) -> u64 {
        const TIME_UNIT: u64 = 100_000_000; // 1 TIME = 100M satoshis
        const BASE_REWARD: f64 = 2000.0;
        const SCALE_FACTOR: f64 = 50.0;

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
        let timestamp = Self::midnight_utc();

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

        // Calculate total weight using 10x scaling
        let total_weight: u64 = masternodes_sorted
            .iter()
            .map(|mn| mn.tier.reward_weight())
            .sum();

        let masternode_pool = (total_reward * 30) / 100;
        let treasury_alloc = (total_reward * 20) / 100;
        let governance_alloc = (total_reward * 10) / 100;
        let _finalizer_pool = total_reward - masternode_pool - treasury_alloc - governance_alloc;

        // Distribute masternode rewards by weight
        let mut masternode_rewards = Vec::new();
        if total_weight > 0 {
            for mn in &masternodes_sorted {
                let weight = mn.tier.reward_weight();
                let reward = (masternode_pool * weight) / total_weight;
                masternode_rewards.push((mn.address.clone(), reward));
            }
        }

        let coinbase_tx = Transaction {
            version: 1,
            inputs: vec![],
            outputs: {
                let mut outs = Vec::new();
                if treasury_alloc > 0 {
                    outs.push(TxOutput {
                        value: treasury_alloc,
                        script_pubkey: b"TREASURY".to_vec(),
                    });
                }
                if governance_alloc > 0 {
                    outs.push(TxOutput {
                        value: governance_alloc,
                        script_pubkey: b"GOVERNANCE".to_vec(),
                    });
                }
                outs
            },
            lock_time: 0,
            timestamp,
        };

        let mut all_txs = vec![coinbase_tx];
        all_txs.extend(txs_sorted);

        let merkle_root = Self::merkle_root(&all_txs);

        let header = BlockHeader {
            version: 1,
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
            treasury_allocation: treasury_alloc,
        }
    }

    fn midnight_utc() -> i64 {
        use chrono::Utc;
        let now = Utc::now();
        let midnight = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
        midnight.and_utc().timestamp()
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
