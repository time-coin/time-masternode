// src/tx_index.rs
//! Transaction index (txindex) for O(1) transaction lookups
//! Maps transaction ID -> (block_height, tx_index_in_block)

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxLocation {
    pub block_height: u64,
    pub tx_index: usize,
}

/// Transaction index for fast transaction lookups
pub struct TransactionIndex {
    db: Arc<sled::Db>,
}

impl TransactionIndex {
    /// Create or open transaction index
    pub fn new(db_path: &str) -> Result<Self, String> {
        let db = sled::open(db_path).map_err(|e| format!("Failed to open txindex: {}", e))?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Add a transaction to the index
    pub fn add_transaction(
        &self,
        txid: &[u8; 32],
        block_height: u64,
        tx_index: usize,
    ) -> Result<(), String> {
        let location = TxLocation {
            block_height,
            tx_index,
        };
        let location_bytes =
            bincode::serialize(&location).map_err(|e| format!("Serialize error: {}", e))?;

        self.db
            .insert(txid, location_bytes)
            .map_err(|e| format!("Failed to insert into txindex: {}", e))?;

        Ok(())
    }

    /// Get transaction location from index
    pub fn get_location(&self, txid: &[u8; 32]) -> Option<TxLocation> {
        self.db
            .get(txid)
            .ok()
            .flatten()
            .and_then(|bytes| bincode::deserialize(&bytes).ok())
    }

    /// Remove a transaction from the index (for reorgs)
    pub fn remove_transaction(&self, txid: &[u8; 32]) -> Result<(), String> {
        self.db
            .remove(txid)
            .map_err(|e| format!("Failed to remove from txindex: {}", e))?;
        Ok(())
    }

    /// Get total number of indexed transactions
    pub fn len(&self) -> usize {
        self.db.len()
    }

    /// Check if index is empty
    pub fn is_empty(&self) -> bool {
        self.db.is_empty()
    }

    /// Flush writes to disk
    pub fn flush(&self) -> Result<(), String> {
        self.db
            .flush()
            .map_err(|e| format!("Failed to flush txindex: {}", e))?;
        Ok(())
    }
}
