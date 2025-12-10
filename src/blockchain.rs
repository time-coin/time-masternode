use crate::block::types::{Block, BlockHeader};
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};
use crate::types::{Transaction, TxOutput};
use crate::vdf::{compute_vdf, VDFConfig, VDFProof};
use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

const BLOCK_TIME_SECONDS: i64 = 600; // 10 minutes
const GENESIS_TIMESTAMP: i64 = 1733011200; // 2025-12-01 00:00:00 UTC
const SATOSHIS_PER_TIME: u64 = 100_000_000;
const BLOCK_REWARD_SATOSHIS: u64 = 100 * SATOSHIS_PER_TIME; // 100 TIME

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisBlock {
    pub network: String,
    pub version: u32,
    pub message: String,
    pub block: Block,
}

pub struct Blockchain {
    storage: sled::Db,
    consensus: Arc<ConsensusEngine>,
    masternode_registry: Arc<MasternodeRegistry>,
    vdf_config: VDFConfig,
    current_height: Arc<RwLock<u64>>,
}

impl Blockchain {
    pub fn new(
        storage: sled::Db,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<MasternodeRegistry>,
        vdf_config: VDFConfig,
    ) -> Self {
        Self {
            storage,
            consensus,
            masternode_registry,
            vdf_config,
            current_height: Arc::new(RwLock::new(0)),
        }
    }

    /// Wait for 3+ masternodes, then create genesis block with initial rewards
    pub async fn initialize_genesis(&self) -> Result<(), String> {
        // Check if genesis already exists
        if self
            .storage
            .contains_key(b"block_0")
            .map_err(|e| e.to_string())?
        {
            let height = self.load_chain_height()?;
            *self.current_height.write().await = height;
            tracing::info!("✓ Genesis block already exists (height: {})", height);
            return Ok(());
        }

        tracing::info!("⏳ Waiting for 3+ masternodes to register...");

        // Wait for 3 masternodes
        loop {
            let active_masternodes = self.masternode_registry.list_active().await;
            let active_count = active_masternodes.len();

            if active_count >= 3 {
                tracing::info!(
                    "✓ {} masternodes registered, creating genesis block",
                    active_count
                );
                break;
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }

        // Create genesis block with rewards for initial masternodes
        let genesis = self.create_genesis_block().await?;
        self.save_block(&genesis)?;
        *self.current_height.write().await = 0;

        tracing::info!("✅ Genesis block created: {}", hex::encode(genesis.hash()));
        Ok(())
    }

    async fn create_genesis_block(&self) -> Result<Block, String> {
        let masternodes = self.masternode_registry.list_active().await;

        let mut outputs = Vec::new();
        let rewards = self.calculate_rewards_from_info(&masternodes);

        for (address, amount) in &rewards {
            outputs.push(TxOutput {
                value: *amount,
                script_pubkey: address.as_bytes().to_vec(),
            });
        }

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs,
            lock_time: 0,
            timestamp: GENESIS_TIMESTAMP,
        };

        let block = Block {
            header: BlockHeader {
                version: 1,
                height: 0,
                previous_hash: [0u8; 32],
                merkle_root: coinbase.txid(),
                timestamp: GENESIS_TIMESTAMP,
                block_reward: BLOCK_REWARD_SATOSHIS,
            },
            transactions: vec![coinbase],
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
            vdf_proof: VDFProof {
                output: vec![0u8; 32],
                iterations: 0,
                checkpoints: vec![],
            },
        };

        Ok(block)
    }

    /// Calculate expected height based on time elapsed since genesis
    pub fn calculate_expected_height(&self) -> u64 {
        let now = Utc::now().timestamp();
        if now < GENESIS_TIMESTAMP {
            return 0;
        }

        let elapsed = now - GENESIS_TIMESTAMP;
        (elapsed / BLOCK_TIME_SECONDS) as u64
    }

    /// Enter catchup mode to create missing blocks
    pub async fn catchup_blocks(&self) -> Result<(), String> {
        let current = *self.current_height.read().await;
        let expected = self.calculate_expected_height();

        if current >= expected {
            tracing::info!("✓ Blockchain is synced (height: {})", current);
            return Ok(());
        }

        tracing::info!("⚡ Entering catchup mode: {} → {}", current, expected);

        for height in (current + 1)..=expected {
            let block_time = GENESIS_TIMESTAMP + (height as i64 * BLOCK_TIME_SECONDS);
            let block = self.create_catchup_block(height, block_time).await?;
            self.save_block(&block)?;
            *self.current_height.write().await = height;

            if height % 100 == 0 {
                tracing::info!("  ⏩ Catchup progress: {}/{}", height, expected);
            }
        }

        tracing::info!("✅ Catchup complete! Height: {}", expected);
        Ok(())
    }

    async fn create_catchup_block(&self, height: u64, timestamp: i64) -> Result<Block, String> {
        let prev_hash = self.get_block_hash(height - 1)?;
        let masternodes = self.masternode_registry.list_active().await;

        let mut outputs = Vec::new();
        let rewards = self.calculate_rewards_from_info(&masternodes);

        for (address, amount) in &rewards {
            outputs.push(TxOutput {
                value: *amount,
                script_pubkey: address.as_bytes().to_vec(),
            });
        }

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs,
            lock_time: 0,
            timestamp,
        };

        let block = Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(),
                timestamp,
                block_reward: BLOCK_REWARD_SATOSHIS,
            },
            transactions: vec![coinbase],
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
            vdf_proof: VDFProof {
                output: vec![0u8; 32],
                iterations: 0,
                checkpoints: vec![],
            },
        };

        Ok(block)
    }

    /// Produce a block at the scheduled time with VDF proof
    pub async fn produce_block(&self) -> Result<Block, String> {
        let height = *self.current_height.read().await + 1;
        let expected = self.calculate_expected_height();

        // Reject future blocks
        if height > expected {
            return Err(format!(
                "Cannot create future block {} (expected: {})",
                height, expected
            ));
        }

        let timestamp = GENESIS_TIMESTAMP + (height as i64 * BLOCK_TIME_SECONDS);
        let now = Utc::now().timestamp();

        // Must be within block time window
        if now < timestamp {
            return Err(format!(
                "Block time not reached ({}s early)",
                timestamp - now
            ));
        }

        if now > timestamp + BLOCK_TIME_SECONDS {
            return Err(format!(
                "Block time window missed ({}s late)",
                now - timestamp
            ));
        }

        // Verify 3+ masternodes
        let masternodes = self.masternode_registry.list_active().await;
        if masternodes.len() < 3 {
            return Err(format!(
                "Insufficient masternodes: {} (need 3)",
                masternodes.len()
            ));
        }

        // Generate VDF proof
        let prev_hash = self.get_block_hash(height - 1)?;
        let vdf_input = prev_hash;
        let vdf_proof = compute_vdf(&vdf_input, &self.vdf_config)?;

        // Get finalized transactions and calculate total fees
        let finalized_txs = self.consensus.get_finalized_transactions_for_block().await;
        let total_fees = self.consensus.tx_pool.get_total_fees().await;

        // Calculate rewards including fees
        let base_reward = BLOCK_REWARD_SATOSHIS;
        let total_reward = base_reward + total_fees;

        let mut outputs = Vec::new();
        let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);

        for (address, amount) in &rewards {
            outputs.push(TxOutput {
                value: *amount,
                script_pubkey: address.as_bytes().to_vec(),
            });
        }

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs,
            lock_time: 0,
            timestamp,
        };

        // Build transaction list: coinbase + finalized transactions
        let mut all_txs = vec![coinbase.clone()];
        all_txs.extend(finalized_txs);

        let block = Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(), // TODO: Calculate proper merkle root
                timestamp,
                block_reward: total_reward,
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
            vdf_proof,
        };

        Ok(block)
    }

    fn calculate_rewards_with_amount(
        &self,
        masternodes: &[MasternodeInfo],
        total_reward: u64,
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let total_weight: u64 = masternodes
            .iter()
            .map(|mn| mn.masternode.tier.reward_weight())
            .sum();

        masternodes
            .iter()
            .map(|mn| {
                let weight = mn.masternode.tier.reward_weight();
                let reward = (total_reward * weight) / total_weight;
                (mn.reward_address.clone(), reward)
            })
            .collect()
    }

    fn calculate_rewards_from_info(
        &self,
        masternodes: &[crate::masternode_registry::MasternodeInfo],
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let total_weight: f64 = masternodes
            .iter()
            .map(|mn| mn.masternode.tier.reward_weight() as f64)
            .sum();

        masternodes
            .iter()
            .map(|mn| {
                let weight = mn.masternode.tier.reward_weight() as f64;
                let share = (weight / total_weight) * (BLOCK_REWARD_SATOSHIS as f64);
                (mn.reward_address.clone(), share as u64)
            })
            .collect()
    }

    pub async fn start_block_production(&self) -> Result<(), String> {
        let blockchain = Arc::new(self.clone());

        tokio::spawn(async move {
            loop {
                // Calculate next block time (on 10-minute boundary)
                let now = Utc::now().timestamp();
                let next_block_time = ((now / BLOCK_TIME_SECONDS) + 1) * BLOCK_TIME_SECONDS;
                let wait_seconds = next_block_time - now;

                tracing::info!(
                    "⏰ Next block in {}s (at {})",
                    wait_seconds,
                    Utc.timestamp_opt(next_block_time, 0).unwrap().to_rfc3339()
                );

                tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds as u64)).await;

                match blockchain.produce_block().await {
                    Ok(block) => {
                        if let Err(e) = blockchain.save_block(&block) {
                            tracing::error!("❌ Failed to save block: {}", e);
                        } else {
                            let mut height = blockchain.current_height.write().await;
                            *height = block.header.height;
                            tracing::info!(
                                "✅ Block {} produced: {} ({} rewards)",
                                block.header.height,
                                hex::encode(block.hash()),
                                block.masternode_rewards.len()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ Block production failed: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    fn save_block(&self, block: &Block) -> Result<(), String> {
        let key = format!("block_{}", block.header.height);
        let value = bincode::serialize(block).map_err(|e| e.to_string())?;
        self.storage
            .insert(key.as_bytes(), value)
            .map_err(|e| e.to_string())?;

        // Update chain tip
        self.storage
            .insert(b"chain_height", &block.header.height.to_le_bytes())
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn get_block(&self, height: u64) -> Result<Block, String> {
        let key = format!("block_{}", height);
        let data = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Block {} not found", height))?;
        bincode::deserialize(&data).map_err(|e| e.to_string())
    }

    fn get_block_hash(&self, height: u64) -> Result<[u8; 32], String> {
        let block = self.get_block(height)?;
        Ok(block.hash())
    }

    fn load_chain_height(&self) -> Result<u64, String> {
        match self
            .storage
            .get(b"chain_height")
            .map_err(|e| e.to_string())?
        {
            Some(bytes) => {
                let arr: [u8; 8] = bytes.as_ref().try_into().map_err(|_| "Invalid height")?;
                Ok(u64::from_le_bytes(arr))
            }
            None => Ok(0),
        }
    }

    pub async fn get_height(&self) -> u64 {
        *self.current_height.read().await
    }
}

impl Clone for Blockchain {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            consensus: self.consensus.clone(),
            masternode_registry: self.masternode_registry.clone(),
            vdf_config: self.vdf_config.clone(),
            current_height: self.current_height.clone(),
        }
    }
}
