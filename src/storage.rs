use crate::block::types::Block;
use crate::types::{OutPoint, UTXO};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[async_trait::async_trait]
#[allow(dead_code)]
pub trait UtxoStorage: Send + Sync {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO>;
    async fn add_utxo(&self, utxo: UTXO) -> Result<(), String>;
    async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), String>;
    async fn list_utxos(&self) -> Vec<UTXO>;
}

#[async_trait::async_trait]
pub trait BlockStorage: Send + Sync {
    async fn get_block(&self, height: u64) -> Option<Block>;
    async fn store_block(&self, block: &Block) -> Result<(), String>;
    async fn get_tip(&self) -> Result<Block, String>;
    async fn get_height(&self) -> u64;
}

pub struct InMemoryUtxoStorage {
    utxos: Arc<RwLock<HashMap<OutPoint, UTXO>>>,
}

impl InMemoryUtxoStorage {
    pub fn new() -> Self {
        Self {
            utxos: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl UtxoStorage for InMemoryUtxoStorage {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        self.utxos.read().await.get(outpoint).cloned()
    }

    async fn add_utxo(&self, utxo: UTXO) -> Result<(), String> {
        self.utxos.write().await.insert(utxo.outpoint.clone(), utxo);
        Ok(())
    }

    async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), String> {
        self.utxos.write().await.remove(outpoint);
        Ok(())
    }

    async fn list_utxos(&self) -> Vec<UTXO> {
        self.utxos.read().await.values().cloned().collect()
    }
}

pub struct SledUtxoStorage {
    db: sled::Db,
}

impl SledUtxoStorage {
    pub fn new(path: &str) -> Result<Self, String> {
        let db = sled::open(path).map_err(|e| e.to_string())?;
        Ok(Self { db })
    }
}

#[async_trait::async_trait]
impl UtxoStorage for SledUtxoStorage {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        let key = bincode::serialize(outpoint).ok()?;
        let value = self.db.get(&key).ok()??;
        bincode::deserialize(&value).ok()
    }

    async fn add_utxo(&self, utxo: UTXO) -> Result<(), String> {
        let key = bincode::serialize(&utxo.outpoint).map_err(|e| e.to_string())?;
        let value = bincode::serialize(&utxo).map_err(|e| e.to_string())?;
        self.db.insert(key, value).map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), String> {
        let key = bincode::serialize(outpoint).map_err(|e| e.to_string())?;
        self.db.remove(key).map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn list_utxos(&self) -> Vec<UTXO> {
        self.db
            .iter()
            .filter_map(|item| {
                let (_, value) = item.ok()?;
                bincode::deserialize(&value).ok()
            })
            .collect()
    }
}

pub struct SledBlockStorage {
    db: sled::Db,
}

impl SledBlockStorage {
    pub fn new(path: &str) -> Result<Self, String> {
        let db = sled::open(path).map_err(|e| e.to_string())?;
        Ok(Self { db })
    }

    pub fn db(&self) -> sled::Db {
        self.db.clone()
    }
}

#[async_trait::async_trait]
impl BlockStorage for SledBlockStorage {
    async fn get_block(&self, height: u64) -> Option<Block> {
        let key = format!("block:{}", height);
        let value = self.db.get(key.as_bytes()).ok()??;
        bincode::deserialize(&value).ok()
    }

    async fn store_block(&self, block: &Block) -> Result<(), String> {
        let key = format!("block:{}", block.header.height);
        let value = bincode::serialize(block).map_err(|e| e.to_string())?;
        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| e.to_string())?;

        // Update tip
        self.db
            .insert(b"tip_height", block.header.height.to_le_bytes().as_ref())
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn get_tip(&self) -> Result<Block, String> {
        let height = self.get_height().await;
        self.get_block(height)
            .await
            .ok_or_else(|| "Tip block not found".to_string())
    }

    async fn get_height(&self) -> u64 {
        self.db
            .get(b"tip_height")
            .ok()
            .flatten()
            .and_then(|bytes| {
                let arr: [u8; 8] = bytes.as_ref().try_into().ok()?;
                Some(u64::from_le_bytes(arr))
            })
            .unwrap_or(0)
    }
}
