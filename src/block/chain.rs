use crate::block::generator::DeterministicBlockGenerator;
use crate::block::genesis::GenesisBlock;
use crate::block::types::Block;
use crate::storage::BlockStorage;
use crate::types::{Hash256, Masternode, Transaction};
use crate::NetworkType;
use std::sync::Arc;
use tokio::sync::RwLock;

#[allow(dead_code)]
pub struct BlockChain {
    storage: Arc<dyn BlockStorage>,
    tip_hash: Arc<RwLock<Hash256>>,
    tip_height: Arc<RwLock<u64>>,
    network: NetworkType,
}

#[allow(dead_code)]
impl BlockChain {
    pub async fn new(storage: Arc<dyn BlockStorage>, network: NetworkType) -> Result<Self, String> {
        // Check if genesis block exists
        let genesis = GenesisBlock::for_network(network);

        if storage.get_block(0).await.is_none() {
            tracing::info!("💎 Initializing blockchain with genesis block");
            storage
                .store_block(&genesis)
                .await
                .map_err(|e| format!("Failed to store genesis block: {}", e))?;
        }

        let tip = storage
            .get_tip()
            .await
            .map_err(|e| format!("Failed to get chain tip: {}", e))?;

        Ok(Self {
            storage,
            tip_hash: Arc::new(RwLock::new(tip.hash())),
            tip_height: Arc::new(RwLock::new(tip.header.height)),
            network,
        })
    }

    pub async fn get_height(&self) -> u64 {
        *self.tip_height.read().await
    }

    pub async fn get_tip_hash(&self) -> Hash256 {
        *self.tip_hash.read().await
    }

    #[allow(dead_code)]
    pub async fn get_block(&self, height: u64) -> Option<Block> {
        self.storage.get_block(height).await
    }

    #[allow(dead_code)]
    pub async fn add_block(&self, block: Block) -> Result<(), String> {
        // Validate block
        let current_height = self.get_height().await;

        if block.header.height != current_height + 1 {
            return Err(format!(
                "Invalid block height: expected {}, got {}",
                current_height + 1,
                block.header.height
            ));
        }

        let current_tip = self.get_tip_hash().await;
        if block.header.previous_hash != current_tip {
            return Err(format!(
                "Invalid previous hash: expected {}, got {}",
                hex::encode(current_tip),
                hex::encode(block.header.previous_hash)
            ));
        }

        // Validate timestamp
        DeterministicBlockGenerator::validate_block_time(block.header.timestamp)?;

        // Store block
        self.storage
            .store_block(&block)
            .await
            .map_err(|e| format!("Failed to store block: {}", e))?;

        // Update tip
        *self.tip_hash.write().await = block.hash();
        *self.tip_height.write().await = block.header.height;

        tracing::info!(
            "✅ Block {} added to chain (hash: {})",
            block.header.height,
            hex::encode(block.hash())
        );

        Ok(())
    }

    /// Generate next block (requires >= 3 masternodes)
    pub async fn generate_next_block(
        &self,
        finalized_txs: Vec<Transaction>,
        active_masternodes: Vec<Masternode>,
    ) -> Result<Block, String> {
        if active_masternodes.len() < 3 {
            return Err(format!(
                "Cannot produce block: need at least 3 masternodes, only {} connected",
                active_masternodes.len()
            ));
        }

        let height = self.get_height().await + 1;
        let previous_hash = self.get_tip_hash().await;
        let timestamp = DeterministicBlockGenerator::next_block_time();

        // Validate timestamp not in future
        DeterministicBlockGenerator::validate_block_time(timestamp)?;

        tracing::info!(
            "🧱 Generating block {} at {} with {} masternodes",
            height,
            timestamp,
            active_masternodes.len()
        );

        let block = DeterministicBlockGenerator::generate(
            height,
            previous_hash,
            finalized_txs,
            active_masternodes,
            0, // Base reward calculated internally
        );

        Ok(block)
    }

    /// Generate catchup blocks to reach expected height
    pub async fn generate_catchup_blocks(
        &self,
        active_masternodes: Vec<Masternode>,
    ) -> Result<Vec<Block>, String> {
        if active_masternodes.len() < 3 {
            return Err("Need at least 3 masternodes for catchup".to_string());
        }

        let current_height = self.get_height().await;
        let expected_height = DeterministicBlockGenerator::calculate_expected_height();

        if current_height >= expected_height {
            return Ok(vec![]);
        }

        tracing::info!(
            "📦 Generating {} catchup blocks from {} to {}",
            expected_height - current_height,
            current_height + 1,
            expected_height
        );

        let mut blocks: Vec<Block> = Vec::new();
        for height in (current_height + 1)..=expected_height {
            let previous_hash = if height == 1 {
                GenesisBlock::for_network(self.network).hash()
            } else if let Some(last_block) = blocks.last() {
                last_block.hash()
            } else {
                self.get_tip_hash().await
            };

            const GENESIS_TIMESTAMP: i64 = 1733011200;
            const BLOCK_TIME_SECONDS: i64 = 600;
            let _timestamp = GENESIS_TIMESTAMP + (height as i64 * BLOCK_TIME_SECONDS);

            let block = DeterministicBlockGenerator::generate(
                height,
                previous_hash,
                vec![], // No transactions in catchup blocks
                active_masternodes.clone(),
                0,
            );

            blocks.push(block);
        }

        Ok(blocks)
    }
}
