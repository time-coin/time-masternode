//! Storage backends for UTXO and blockchain data.
//!
//! Provides both in-memory and persistent (sled) storage options.
//! The SledUtxoStorage is an alternative backend that's currently unused
//! but available for future use if persistent UTXO storage is needed.

use crate::block::types::Block;
use crate::types::{OutPoint, UTXO};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::spawn_blocking;

/// Compression level for zstd (1-22, higher = better ratio but slower)
/// Level 3 is a good balance of speed and compression
const ZSTD_COMPRESSION_LEVEL: i32 = 3;

/// Magic bytes to identify compressed blocks (to handle mixed storage)
const COMPRESSED_MAGIC: &[u8; 4] = b"ZSTD";

/// Compress data using zstd
pub fn compress_block(data: &[u8]) -> Vec<u8> {
    let compressed =
        zstd::encode_all(data, ZSTD_COMPRESSION_LEVEL).unwrap_or_else(|_| data.to_vec());
    // Prepend magic bytes so we can detect compressed vs uncompressed
    let mut result = Vec::with_capacity(4 + compressed.len());
    result.extend_from_slice(COMPRESSED_MAGIC);
    result.extend_from_slice(&compressed);
    result
}

/// Decompress data, handling both compressed and uncompressed formats
pub fn decompress_block(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() > 4 && &data[0..4] == COMPRESSED_MAGIC {
        // Compressed data - decompress
        zstd::decode_all(&data[4..]).map_err(|e| format!("Decompression failed: {}", e))
    } else {
        // Uncompressed data (legacy) - return as-is
        Ok(data.to_vec())
    }
}

/// Check if compression is beneficial (don't compress if it makes data larger)
pub fn should_compress(original: &[u8], compressed: &[u8]) -> bool {
    // Only use compression if it saves at least 10%
    compressed.len() < (original.len() * 9 / 10)
}

#[derive(thiserror::Error, Debug)]
pub enum StorageError {
    #[error("Serialization failed: {0}")]
    Serialization(#[from] bincode::Error),

    #[error("Database error: {0}")]
    Database(#[from] sled::Error),

    #[error("Task join error: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),

    #[error("UTXO not found: {0:?}")]
    #[allow(dead_code)]
    NotFound(OutPoint),
}

#[async_trait::async_trait]
#[allow(dead_code)]
pub trait UtxoStorage: Send + Sync {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO>;
    async fn add_utxo(&self, utxo: UTXO) -> Result<(), StorageError>;
    async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), StorageError>;
    async fn list_utxos(&self) -> Vec<UTXO>;
    async fn batch_update(&self, add: Vec<UTXO>, remove: Vec<OutPoint>)
        -> Result<(), StorageError>;
}

#[async_trait::async_trait]
#[allow(dead_code)]
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

impl Default for InMemoryUtxoStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl UtxoStorage for InMemoryUtxoStorage {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        self.utxos.read().await.get(outpoint).cloned()
    }

    async fn add_utxo(&self, utxo: UTXO) -> Result<(), StorageError> {
        self.utxos.write().await.insert(utxo.outpoint.clone(), utxo);
        Ok(())
    }

    async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), StorageError> {
        self.utxos.write().await.remove(outpoint);
        Ok(())
    }

    async fn list_utxos(&self) -> Vec<UTXO> {
        self.utxos.read().await.values().cloned().collect()
    }

    async fn batch_update(
        &self,
        add: Vec<UTXO>,
        remove: Vec<OutPoint>,
    ) -> Result<(), StorageError> {
        let mut utxos = self.utxos.write().await;
        for utxo in add {
            utxos.insert(utxo.outpoint.clone(), utxo);
        }
        for outpoint in remove {
            utxos.remove(&outpoint);
        }
        Ok(())
    }
}

/// Sled-based UTXO storage backend.
///
/// Note: This is an alternative to InMemoryUtxoStorage for persistent storage.
/// Currently unused because UTXO storage is handled via the main sled database
/// in blockchain.rs. This could be used for a dedicated UTXO database.
#[allow(dead_code)]
pub struct SledUtxoStorage {
    db: sled::Db,
}

#[allow(dead_code)] // Used by binary (main.rs) for persistent storage option
impl SledUtxoStorage {
    pub fn new(path: &str) -> Result<Self, StorageError> {
        use sysinfo::{MemoryRefreshKind, RefreshKind, System};

        let sys = System::new_with_specifics(
            RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
        );
        let available_memory = sys.available_memory();
        let cache_size = std::cmp::min(available_memory / 10, 512 * 1024 * 1024);

        let db = sled::Config::new()
            .path(path)
            .cache_capacity(cache_size)
            .flush_every_ms(Some(1000))
            .mode(sled::Mode::HighThroughput)
            .open()?;

        Ok(Self { db })
    }

    #[allow(dead_code)]
    pub fn db(&self) -> sled::Db {
        self.db.clone()
    }
}

#[async_trait::async_trait]
impl UtxoStorage for SledUtxoStorage {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        let db = self.db.clone();
        let key = bincode::serialize(outpoint).ok()?;

        spawn_blocking(move || {
            let value = db.get(&key).ok()??;
            bincode::deserialize(&value).ok()
        })
        .await
        .ok()
        .flatten()
    }

    async fn add_utxo(&self, utxo: UTXO) -> Result<(), StorageError> {
        let db = self.db.clone();
        let key = bincode::serialize(&utxo.outpoint)?;
        let value = bincode::serialize(&utxo)?;

        spawn_blocking(move || db.insert(key, value))
            .await
            .map_err(StorageError::TaskJoin)??;

        Ok(())
    }

    async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), StorageError> {
        let db = self.db.clone();
        let key = bincode::serialize(outpoint)?;

        spawn_blocking(move || db.remove(key))
            .await
            .map_err(StorageError::TaskJoin)??;

        Ok(())
    }

    async fn list_utxos(&self) -> Vec<UTXO> {
        let db = self.db.clone();

        match spawn_blocking(move || {
            db.iter()
                .filter_map(|item| {
                    let (_, value) = item.ok()?;
                    bincode::deserialize(&value).ok()
                })
                .collect::<Vec<_>>()
        })
        .await
        {
            Ok(utxos) => utxos,
            Err(e) => {
                tracing::error!("Failed to list UTXOs: {}", e);
                Vec::new()
            }
        }
    }

    async fn batch_update(
        &self,
        add: Vec<UTXO>,
        remove: Vec<OutPoint>,
    ) -> Result<(), StorageError> {
        let db = self.db.clone();

        spawn_blocking(move || {
            let mut batch = sled::Batch::default();

            for outpoint in remove {
                let key = bincode::serialize(&outpoint)?;
                batch.remove(key);
            }

            for utxo in add {
                let key = bincode::serialize(&utxo.outpoint)?;
                let value = bincode::serialize(&utxo)?;
                batch.insert(key, value);
            }

            db.apply_batch(batch)?;
            Ok::<_, StorageError>(())
        })
        .await
        .map_err(StorageError::TaskJoin)?
    }
}

#[allow(dead_code)]
pub struct SledBlockStorage {
    db: sled::Db,
}

#[allow(dead_code)]
impl SledBlockStorage {
    pub fn new(path: &str) -> Result<Self, StorageError> {
        use sysinfo::{MemoryRefreshKind, RefreshKind, System};

        let sys = System::new_with_specifics(
            RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
        );
        let available_memory = sys.available_memory();
        let cache_size = std::cmp::min(available_memory / 10, 512 * 1024 * 1024);

        tracing::info!(
            cache_mb = cache_size / (1024 * 1024),
            available_mb = available_memory / (1024 * 1024),
            "Configured sled cache"
        );

        let db = sled::Config::new()
            .path(path)
            .cache_capacity(cache_size)
            .flush_every_ms(Some(1000))
            .mode(sled::Mode::HighThroughput)
            .open()?;

        Ok(Self { db })
    }

    pub fn db(&self) -> sled::Db {
        self.db.clone()
    }
}

#[async_trait::async_trait]
impl BlockStorage for SledBlockStorage {
    async fn get_block(&self, height: u64) -> Option<Block> {
        let db = self.db.clone();
        let key_new = format!("block_{}", height);
        let key_old = format!("block:{}", height);

        spawn_blocking(move || {
            // Try new format first
            if let Ok(Some(value)) = db.get(key_new.as_bytes()) {
                return bincode::deserialize(&value).ok();
            }
            // Fallback to old format for backward compatibility
            if let Ok(Some(value)) = db.get(key_old.as_bytes()) {
                return bincode::deserialize(&value).ok();
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    async fn store_block(&self, block: &Block) -> Result<(), String> {
        let db = self.db.clone();
        let block = block.clone();
        let key = format!("block_{}", block.header.height);

        spawn_blocking(move || {
            let value = bincode::serialize(&block)?;
            db.insert(key.as_bytes(), value)?;
            db.insert(b"tip_height", block.header.height.to_le_bytes().as_ref())?;
            // CRITICAL: Flush to disk to prevent data loss
            db.flush()?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
    }

    async fn get_tip(&self) -> Result<Block, String> {
        let height = self.get_height().await;
        self.get_block(height)
            .await
            .ok_or_else(|| "Tip block not found".to_string())
    }

    async fn get_height(&self) -> u64 {
        let db = self.db.clone();

        spawn_blocking(move || {
            db.get(b"tip_height")
                .ok()
                .flatten()
                .and_then(|bytes| {
                    let arr: [u8; 8] = bytes.as_ref().try_into().ok()?;
                    Some(u64::from_le_bytes(arr))
                })
                .unwrap_or(0)
        })
        .await
        .unwrap_or(0)
    }
}
