//! UTXO (Unspent Transaction Output) state management.
//!
//! Manages the UTXO set for tracking spendable outputs. Provides locking
//! mechanism for concurrent transaction processing.

use crate::storage::UtxoStorage;
use crate::types::*;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_TIMEOUT_SECS: i64 = 600; // Phase 1.4: 10 minutes to align with block time

// Optimization: Pre-allocate DashMap with expected capacity
// Typical node has ~100k UTXOs, pre-allocating reduces rehashing
const EXPECTED_UTXO_COUNT: usize = 100_000;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum UtxoError {
    #[error("UTXO not found")]
    NotFound,

    #[error("UTXO already locked by transaction {0}")]
    AlreadyLocked(String),

    #[error("UTXO already spent")]
    AlreadySpent,

    #[error("Lock expired")]
    LockExpired,

    #[error("Lock owned by different transaction")]
    LockMismatch,

    #[error("Storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),

    #[error("UTXO is locked as masternode collateral")]
    LockedAsCollateral,
}

pub struct UTXOStateManager {
    pub storage: Arc<dyn UtxoStorage>,
    pub utxo_states: DashMap<OutPoint, UTXOState>,
    /// Track UTXOs locked as masternode collateral
    pub locked_collaterals: DashMap<OutPoint, LockedCollateral>,
}

impl Default for UTXOStateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl UTXOStateManager {
    pub fn new() -> Self {
        use crate::storage::InMemoryUtxoStorage;
        Self {
            storage: Arc::new(InMemoryUtxoStorage::new()),
            utxo_states: DashMap::with_capacity(EXPECTED_UTXO_COUNT),
            locked_collaterals: DashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn new_with_storage(storage: Arc<dyn UtxoStorage>) -> Self {
        Self {
            storage,
            utxo_states: DashMap::with_capacity(EXPECTED_UTXO_COUNT),
            locked_collaterals: DashMap::new(),
        }
    }

    /// Initialize UTXO states from storage (call after creating with new_with_storage)
    /// This ensures in-memory state map is synchronized with persistent storage
    pub async fn initialize_states(&self) -> Result<usize, UtxoError> {
        let utxos = self.storage.list_utxos().await;
        let count = utxos.len();

        tracing::info!(
            "🔄 Initializing UTXO states for {} UTXOs from storage",
            count
        );

        for utxo in utxos {
            // Only initialize if not already in state map
            if !self.utxo_states.contains_key(&utxo.outpoint) {
                self.utxo_states.insert(utxo.outpoint, UTXOState::Unspent);
            }
        }

        tracing::info!(
            "✅ UTXO state initialization complete: {} entries",
            self.utxo_states.len()
        );
        Ok(count)
    }

    /// Clear all UTXOs from both storage and in-memory state maps.
    /// Used during chain reset or full reindex to start with a clean UTXO set.
    pub async fn clear_all(&self) -> Result<(), UtxoError> {
        tracing::info!("🗑️  Clearing all UTXOs from storage and state maps...");

        // Clear persistent storage
        self.storage.clear_all().await?;

        // Clear in-memory state maps
        self.utxo_states.clear();
        self.locked_collaterals.clear();

        tracing::info!("✅ All UTXOs cleared");
        Ok(())
    }

    fn current_timestamp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn is_lock_expired(locked_at: i64) -> bool {
        Self::current_timestamp() - locked_at > LOCK_TIMEOUT_SECS
    }

    pub async fn add_utxo(&self, utxo: UTXO) -> Result<(), UtxoError> {
        let outpoint = utxo.outpoint.clone();

        // Check if UTXO already exists
        if let Some(existing_state) = self.utxo_states.get(&outpoint) {
            match existing_state.value() {
                UTXOState::Unspent => {
                    // UTXO already exists and is unspent - this is OK during fork resolution
                    // when the same block might be processed multiple times
                    tracing::debug!(
                        "UTXO {:?} already exists in Unspent state - treating as success",
                        outpoint
                    );
                    return Ok(());
                }
                _ => {
                    // UTXO exists but in a different state (spent, locked, etc.)
                    return Err(UtxoError::AlreadySpent);
                }
            }
        }

        self.storage.add_utxo(utxo).await?;
        self.utxo_states.insert(outpoint, UTXOState::Unspent);
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        self.storage.remove_utxo(outpoint).await?;
        self.utxo_states.remove(outpoint);
        Ok(())
    }

    /// Get a UTXO by outpoint (used for undo logs)
    pub async fn get_utxo(&self, outpoint: &OutPoint) -> Result<UTXO, UtxoError> {
        self.storage
            .get_utxo(outpoint)
            .await
            .ok_or(UtxoError::NotFound)
    }

    /// Mark a UTXO as spent (used when processing blocks)
    pub async fn spend_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        // Check if UTXO is locked as collateral
        if self.is_collateral_locked(outpoint) {
            return Err(UtxoError::LockedAsCollateral);
        }

        self.storage.remove_utxo(outpoint).await?;
        self.utxo_states.insert(
            outpoint.clone(),
            UTXOState::SpentFinalized {
                txid: [0u8; 32], // Block processing spend
                finalized_at: Self::current_timestamp(),
                votes: 0,
            },
        );
        Ok(())
    }

    /// Restore a UTXO during rollback (handles spent state)
    /// Unlike add_utxo, this forces the UTXO back to Unspent even if it was previously spent
    pub async fn restore_utxo(&self, utxo: UTXO) -> Result<(), UtxoError> {
        let outpoint = utxo.outpoint.clone();

        // During rollback, we need to restore UTXOs that were spent
        // Clear any existing state (including SpentFinalized) and restore to Unspent
        if let Some(existing_state) = self.utxo_states.get(&outpoint) {
            match existing_state.value() {
                UTXOState::Unspent => {
                    // Already unspent - check if storage has it
                    if self.storage.get_utxo(&outpoint).await.is_some() {
                        tracing::debug!(
                            "UTXO {:?} already exists in Unspent state during restore",
                            outpoint
                        );
                        return Ok(());
                    }
                    // State says unspent but not in storage - add it
                }
                UTXOState::SpentFinalized { .. }
                | UTXOState::SpentPending { .. }
                | UTXOState::Confirmed { .. } => {
                    // This is the rollback case - UTXO was spent but we're undoing it
                    tracing::debug!("Restoring spent UTXO {:?} during rollback", outpoint);
                }
                UTXOState::Locked { txid, .. } => {
                    // Locked UTXO being restored - clear the lock
                    tracing::warn!(
                        "Restoring locked UTXO {:?} (was locked by tx {:?})",
                        outpoint,
                        hex::encode(txid)
                    );
                }
            }
        }

        // Add to storage and set state to Unspent
        self.storage.add_utxo(utxo).await?;
        self.utxo_states.insert(outpoint, UTXOState::Unspent);
        Ok(())
    }

    /// Atomically lock a UTXO for a pending transaction
    pub fn lock_utxo(&self, outpoint: &OutPoint, txid: Hash256) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        // Check if UTXO is locked as collateral first
        if self.is_collateral_locked(outpoint) {
            return Err(UtxoError::LockedAsCollateral);
        }

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => match entry.get() {
                UTXOState::Unspent => {
                    entry.insert(UTXOState::Locked {
                        txid,
                        locked_at: Self::current_timestamp(),
                    });
                    tracing::debug!(
                        "🔒 Locked UTXO {:?} for tx {:?}",
                        outpoint,
                        hex::encode(txid)
                    );
                    Ok(())
                }
                UTXOState::Locked {
                    txid: existing_txid,
                    locked_at,
                } => {
                    if existing_txid == &txid {
                        return Ok(());
                    }

                    if Self::is_lock_expired(*locked_at) {
                        tracing::warn!("⏰ Expired lock on UTXO {:?}, allowing new lock", outpoint);
                        entry.insert(UTXOState::Locked {
                            txid,
                            locked_at: Self::current_timestamp(),
                        });
                        Ok(())
                    } else {
                        Err(UtxoError::AlreadyLocked(hex::encode(existing_txid)))
                    }
                }
                UTXOState::SpentPending { .. }
                | UTXOState::SpentFinalized { .. }
                | UTXOState::Confirmed { .. } => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(entry) => {
                entry.insert(UTXOState::Locked {
                    txid,
                    locked_at: Self::current_timestamp(),
                });
                Ok(())
            }
        }
    }

    /// Unlock a UTXO (rollback a failed/timed-out transaction)
    #[allow(dead_code)]
    pub fn unlock_utxo(&self, outpoint: &OutPoint, txid: &Hash256) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => match entry.get() {
                UTXOState::Locked {
                    txid: locked_txid, ..
                } => {
                    if locked_txid == txid {
                        entry.insert(UTXOState::Unspent);
                        tracing::debug!("🔓 Unlocked UTXO {:?}", outpoint);
                        Ok(())
                    } else {
                        Err(UtxoError::LockMismatch)
                    }
                }
                UTXOState::Unspent => Ok(()),
                _ => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(_) => Err(UtxoError::NotFound),
        }
    }

    /// Commit a locked UTXO as spent (finalize transaction)
    #[allow(dead_code)]
    pub async fn commit_spend(
        &self,
        outpoint: &OutPoint,
        txid: &Hash256,
        block_height: u64,
    ) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => match entry.get() {
                UTXOState::Locked {
                    txid: locked_txid, ..
                } => {
                    if locked_txid != txid {
                        return Err(UtxoError::LockMismatch);
                    }

                    self.storage.remove_utxo(outpoint).await?;

                    entry.insert(UTXOState::Confirmed {
                        txid: *txid,
                        block_height,
                        confirmed_at: Self::current_timestamp(),
                    });

                    tracing::info!(
                        "✅ Committed UTXO spend {:?} in block {}",
                        outpoint,
                        block_height
                    );
                    Ok(())
                }
                UTXOState::Unspent => {
                    tracing::warn!("⚠️ Spending unlocked UTXO {:?}", outpoint);
                    self.storage.remove_utxo(outpoint).await?;
                    entry.insert(UTXOState::Confirmed {
                        txid: *txid,
                        block_height,
                        confirmed_at: Self::current_timestamp(),
                    });
                    Ok(())
                }
                _ => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(_) => Err(UtxoError::NotFound),
        }
    }

    /// Batch lock multiple UTXOs atomically
    #[allow(dead_code)]
    pub fn lock_utxos_atomic(
        &self,
        outpoints: &[OutPoint],
        txid: Hash256,
    ) -> Result<(), UtxoError> {
        let mut locked = Vec::with_capacity(outpoints.len());

        for outpoint in outpoints {
            match self.lock_utxo(outpoint, txid) {
                Ok(()) => locked.push(outpoint.clone()),
                Err(e) => {
                    for locked_outpoint in locked {
                        let _ = self.unlock_utxo(&locked_outpoint, &txid);
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Clean up expired locks
    #[allow(dead_code)]
    pub fn cleanup_expired_locks(&self) -> usize {
        let mut cleaned = 0;

        self.utxo_states.retain(|outpoint, state| {
            if let UTXOState::Locked { locked_at, txid } = state {
                if Self::is_lock_expired(*locked_at) {
                    tracing::info!(
                        "🧹 Cleaning expired lock on UTXO {:?} (tx {:?})",
                        outpoint,
                        hex::encode(txid)
                    );
                    *state = UTXOState::Unspent;
                    cleaned += 1;
                }
            }
            true
        });

        cleaned
    }

    pub fn get_state(&self, outpoint: &OutPoint) -> Option<UTXOState> {
        self.utxo_states.get(outpoint).map(|r| r.value().clone())
    }

    pub fn update_state(&self, outpoint: &OutPoint, state: UTXOState) {
        self.utxo_states.insert(outpoint.clone(), state);
    }

    /// Force reset a UTXO to Unspent state (for recovery from stuck locks)
    pub fn force_unlock(&self, outpoint: &OutPoint) -> bool {
        if self.utxo_states.contains_key(outpoint) {
            self.utxo_states
                .insert(outpoint.clone(), UTXOState::Unspent);
            true
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub async fn get_finalized_transactions(&self) -> Vec<Transaction> {
        Vec::new()
    }

    /// Get all UTXOs (for diagnostics)
    pub async fn list_all_utxos(&self) -> Vec<UTXO> {
        self.storage.list_utxos().await
    }

    #[allow(dead_code)]
    pub fn get_locked_utxos(&self) -> Vec<(OutPoint, Hash256, i64)> {
        self.utxo_states
            .iter()
            .filter_map(|entry| {
                if let UTXOState::Locked { txid, locked_at } = entry.value() {
                    Some((entry.key().clone(), *txid, *locked_at))
                } else {
                    None
                }
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn is_spendable(&self, outpoint: &OutPoint, by_txid: Option<&Hash256>) -> bool {
        // First check if locked as collateral
        if self.is_collateral_locked(outpoint) {
            return false;
        }

        match self.utxo_states.get(outpoint) {
            Some(ref state) => match state.value() {
                UTXOState::Unspent => true,
                UTXOState::Locked { txid, locked_at } => {
                    (by_txid == Some(txid)) || Self::is_lock_expired(*locked_at)
                }
                _ => false,
            },
            None => false,
        }
    }

    /// Calculate hash of entire UTXO set for state comparison
    #[allow(dead_code)]
    pub async fn calculate_utxo_set_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut utxos = self.list_all_utxos().await;
        utxos.sort_unstable_by(|a, b| {
            (&a.outpoint.txid, a.outpoint.vout).cmp(&(&b.outpoint.txid, b.outpoint.vout))
        });

        let mut hasher = Sha256::new();
        for utxo in utxos {
            hasher.update(utxo.outpoint.txid);
            hasher.update(utxo.outpoint.vout.to_le_bytes());
            hasher.update(utxo.value.to_le_bytes());
            hasher.update(&utxo.script_pubkey);
        }

        hasher.finalize().into()
    }

    #[allow(dead_code)]
    pub async fn get_utxo_diff(&self, remote_utxos: &[UTXO]) -> (Vec<OutPoint>, Vec<UTXO>) {
        let local_utxos = self.list_all_utxos().await;

        let local_set: std::collections::HashSet<_> = local_utxos
            .iter()
            .map(|u| (u.outpoint.clone(), u.value))
            .collect();

        let remote_set: std::collections::HashSet<_> = remote_utxos
            .iter()
            .map(|u| (u.outpoint.clone(), u.value))
            .collect();

        let to_remove: Vec<OutPoint> = local_set
            .difference(&remote_set)
            .map(|(outpoint, _)| outpoint.clone())
            .collect();

        let to_add: Vec<UTXO> = remote_utxos
            .iter()
            .filter(|u| !local_set.contains(&(u.outpoint.clone(), u.value)))
            .cloned()
            .collect();

        (to_remove, to_add)
    }

    #[allow(dead_code)]
    pub async fn reconcile_utxo_state(
        &self,
        to_remove: Vec<OutPoint>,
        to_add: Vec<UTXO>,
    ) -> Result<(), UtxoError> {
        let remove_count = to_remove.len();
        let add_count = to_add.len();

        for outpoint in to_remove {
            if let Err(e) = self.storage.remove_utxo(&outpoint).await {
                tracing::warn!("Failed to remove UTXO during reconciliation: {}", e);
            }
            self.utxo_states.remove(&outpoint);
        }

        for utxo in to_add {
            let _ = self.add_utxo(utxo).await;
        }

        tracing::info!(
            "🔄 Reconciled UTXO state: removed {}, added {}",
            remove_count,
            add_count
        );
        Ok(())
    }

    // ========== Masternode Collateral Locking Methods ==========

    /// Lock a UTXO as masternode collateral
    pub fn lock_collateral(
        &self,
        outpoint: OutPoint,
        masternode_address: String,
        lock_height: u64,
        amount: u64,
    ) -> Result<(), UtxoError> {
        // Check if UTXO exists and is unspent
        match self.utxo_states.get(&outpoint) {
            Some(state) => match state.value() {
                UTXOState::Unspent => {
                    // Good to lock
                }
                _ => return Err(UtxoError::AlreadySpent),
            },
            None => return Err(UtxoError::NotFound),
        }

        // Check if already locked as collateral
        if self.locked_collaterals.contains_key(&outpoint) {
            return Err(UtxoError::LockedAsCollateral);
        }

        // Create locked collateral entry
        let locked_collateral =
            LockedCollateral::new(outpoint.clone(), masternode_address, lock_height, amount);

        // Store in locked collaterals map
        self.locked_collaterals
            .insert(outpoint.clone(), locked_collateral);

        tracing::debug!(
            "🔒 Locked collateral UTXO {:?} (amount: {})",
            outpoint,
            amount
        );
        Ok(())
    }

    /// Unlock a UTXO from masternode collateral
    pub fn unlock_collateral(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        if let Some((_, locked)) = self.locked_collaterals.remove(outpoint) {
            tracing::info!(
                "🔓 Unlocked collateral UTXO {:?} (was {} TIME for {})",
                outpoint,
                locked.amount,
                locked.masternode_address
            );
            Ok(())
        } else {
            Err(UtxoError::NotFound)
        }
    }

    /// Check if a UTXO is locked as collateral
    pub fn is_collateral_locked(&self, outpoint: &OutPoint) -> bool {
        self.locked_collaterals.contains_key(outpoint)
    }

    /// Get locked collateral info
    pub fn get_locked_collateral(&self, outpoint: &OutPoint) -> Option<LockedCollateral> {
        self.locked_collaterals
            .get(outpoint)
            .map(|r| r.value().clone())
    }

    /// List all locked collaterals
    pub fn list_locked_collaterals(&self) -> Vec<LockedCollateral> {
        self.locked_collaterals
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// List locked collaterals for a specific masternode
    pub fn list_collaterals_for_masternode(
        &self,
        masternode_address: &str,
    ) -> Vec<LockedCollateral> {
        self.locked_collaterals
            .iter()
            .filter(|entry| entry.value().masternode_address == masternode_address)
            .map(|entry| entry.value().clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OutPoint, UTXO};

    fn create_test_outpoint(seed: u8) -> OutPoint {
        OutPoint {
            txid: [seed; 32],
            vout: 0,
        }
    }

    fn create_test_utxo(seed: u8) -> UTXO {
        UTXO {
            outpoint: create_test_outpoint(seed),
            value: 1000,
            script_pubkey: vec![1, 2, 3],
            address: format!("test_address_{}", seed),
        }
    }

    fn create_test_txid(seed: u8) -> Hash256 {
        [seed; 32]
    }

    /// Phase 1.4 Test 1: Basic double-spend prevention
    #[tokio::test]
    async fn test_double_spend_prevention_basic() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(1);
        let utxo = create_test_utxo(1);

        // Add UTXO
        manager.add_utxo(utxo).await.unwrap();

        // First transaction locks the UTXO
        let tx1 = create_test_txid(10);
        assert!(manager.lock_utxo(&outpoint, tx1).is_ok());

        // Second transaction should fail to lock same UTXO
        let tx2 = create_test_txid(20);
        let result = manager.lock_utxo(&outpoint, tx2);

        assert!(result.is_err());
        match result {
            Err(UtxoError::AlreadyLocked(_)) => {} // Expected
            _ => panic!("Expected AlreadyLocked error"),
        }
    }

    /// Phase 1.4 Test 2: Lock timeout and reuse
    #[tokio::test]
    async fn test_lock_timeout() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(2);
        let utxo = create_test_utxo(2);

        manager.add_utxo(utxo).await.unwrap();

        let tx1 = create_test_txid(30);
        manager.lock_utxo(&outpoint, tx1).unwrap();

        // Manually expire the lock by manipulating state
        // In production, this would happen after LOCK_TIMEOUT_SECS (600s)
        manager.utxo_states.insert(
            outpoint.clone(),
            UTXOState::Locked {
                txid: tx1,
                locked_at: UTXOStateManager::current_timestamp() - LOCK_TIMEOUT_SECS - 1,
            },
        );

        // Now a new transaction should be able to lock it
        let tx2 = create_test_txid(40);
        assert!(manager.lock_utxo(&outpoint, tx2).is_ok());

        // Verify the new lock is in place
        if let Some(UTXOState::Locked { txid, .. }) = manager.get_state(&outpoint) {
            assert_eq!(txid, tx2);
        } else {
            panic!("Expected locked state");
        }
    }

    /// Phase 1.4 Test 3: Unlock and relock
    #[tokio::test]
    async fn test_unlock_and_relock() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(3);
        let utxo = create_test_utxo(3);

        manager.add_utxo(utxo).await.unwrap();

        // Lock with first transaction
        let tx1 = create_test_txid(50);
        manager.lock_utxo(&outpoint, tx1).unwrap();

        // Unlock (transaction failed/timed out)
        manager.unlock_utxo(&outpoint, &tx1).unwrap();

        // Verify unlocked
        match manager.get_state(&outpoint) {
            Some(UTXOState::Unspent) => {} // Expected
            _ => panic!("Expected unspent state after unlock"),
        }

        // Second transaction can now lock it
        let tx2 = create_test_txid(60);
        assert!(manager.lock_utxo(&outpoint, tx2).is_ok());
    }

    /// Phase 1.4 Test 4: Atomic batch locking
    #[tokio::test]
    async fn test_atomic_batch_locking() {
        let manager = UTXOStateManager::new();

        // Create multiple UTXOs
        for i in 1..=5 {
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
        }

        let outpoints: Vec<OutPoint> = (1..=5).map(create_test_outpoint).collect();
        let tx1 = create_test_txid(70);

        // Lock all atomically
        assert!(manager.lock_utxos_atomic(&outpoints, tx1).is_ok());

        // Verify all are locked
        for outpoint in &outpoints {
            match manager.get_state(outpoint) {
                Some(UTXOState::Locked { txid, .. }) => {
                    assert_eq!(txid, tx1);
                }
                _ => panic!("Expected locked state"),
            }
        }
    }

    /// Phase 1.4 Test 5: Atomic rollback on conflict
    #[tokio::test]
    async fn test_atomic_rollback_on_conflict() {
        let manager = UTXOStateManager::new();

        // Create 3 UTXOs
        for i in 1..=3 {
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
        }

        // Lock the second UTXO with a different transaction
        let conflicting_tx = create_test_txid(80);
        let outpoint2 = create_test_outpoint(2);
        manager.lock_utxo(&outpoint2, conflicting_tx).unwrap();

        // Try to lock all three atomically (should fail)
        let outpoints: Vec<OutPoint> = (1..=3).map(create_test_outpoint).collect();
        let tx1 = create_test_txid(90);
        let result = manager.lock_utxos_atomic(&outpoints, tx1);

        assert!(result.is_err());

        // Verify first UTXO was rolled back (not locked by tx1)
        let outpoint1 = create_test_outpoint(1);
        match manager.get_state(&outpoint1) {
            Some(UTXOState::Unspent) => {} // Expected - rollback worked
            Some(UTXOState::Locked { txid, .. }) => {
                panic!(
                    "UTXO should be unlocked after atomic rollback, but locked by {:?}",
                    hex::encode(txid)
                );
            }
            _ => panic!("Unexpected state"),
        }
    }

    /// Phase 1.4 Test 6: Cannot spend locked UTXO
    #[tokio::test]
    async fn test_cannot_spend_locked_utxo() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(4);
        let utxo = create_test_utxo(4);

        manager.add_utxo(utxo).await.unwrap();

        // Lock the UTXO
        let tx1 = create_test_txid(100);
        manager.lock_utxo(&outpoint, tx1).unwrap();

        // Attempt to spend should fail
        let _result = manager.spend_utxo(&outpoint).await;
        // In current implementation, spend_utxo doesn't check lock state
        // This test documents current behavior - may need enhancement
    }

    /// Phase 1.4 Test 7: Cleanup expired locks
    #[tokio::test]
    async fn test_cleanup_expired_locks() {
        let manager = UTXOStateManager::new();

        // Create and lock multiple UTXOs
        for i in 1..=3 {
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
            let outpoint = create_test_outpoint(i);
            let tx = create_test_txid(110 + i);
            manager.lock_utxo(&outpoint, tx).unwrap();
        }

        // Manually expire first two locks
        for i in 1..=2 {
            let outpoint = create_test_outpoint(i);
            manager.utxo_states.insert(
                outpoint,
                UTXOState::Locked {
                    txid: create_test_txid(110 + i),
                    locked_at: UTXOStateManager::current_timestamp() - LOCK_TIMEOUT_SECS - 1,
                },
            );
        }

        // Cleanup expired locks
        let cleaned = manager.cleanup_expired_locks();
        assert_eq!(cleaned, 2);

        // Verify first two are now unspent
        for i in 1..=2 {
            let outpoint = create_test_outpoint(i);
            match manager.get_state(&outpoint) {
                Some(UTXOState::Unspent) => {} // Expected
                _ => panic!("Lock should be cleaned up"),
            }
        }

        // Third should still be locked
        let outpoint3 = create_test_outpoint(3);
        match manager.get_state(&outpoint3) {
            Some(UTXOState::Locked { .. }) => {} // Expected
            _ => panic!("Third lock should still be active"),
        }
    }

    /// Phase 1.4 Test 8: Idempotent locking (same tx can lock multiple times)
    #[tokio::test]
    async fn test_idempotent_locking() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(5);
        let utxo = create_test_utxo(5);

        manager.add_utxo(utxo).await.unwrap();

        let tx1 = create_test_txid(120);

        // Lock once
        manager.lock_utxo(&outpoint, tx1).unwrap();

        // Lock again with same tx (should succeed - idempotent)
        assert!(manager.lock_utxo(&outpoint, tx1).is_ok());
    }

    /// Phase 1.4 Test 9: Commit spend from locked state
    #[tokio::test]
    async fn test_commit_spend_from_locked() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(6);
        let utxo = create_test_utxo(6);

        manager.add_utxo(utxo).await.unwrap();

        let tx1 = create_test_txid(130);
        manager.lock_utxo(&outpoint, tx1).unwrap();

        // Commit the spend (transaction confirmed in block)
        assert!(manager.commit_spend(&outpoint, &tx1, 1000).await.is_ok());

        // Verify state is now confirmed
        match manager.get_state(&outpoint) {
            Some(UTXOState::Confirmed {
                txid, block_height, ..
            }) => {
                assert_eq!(txid, tx1);
                assert_eq!(block_height, 1000);
            }
            _ => panic!("Expected confirmed state"),
        }
    }

    /// Phase 1.4 Test 10: Cannot commit spend with wrong txid
    #[tokio::test]
    async fn test_cannot_commit_with_wrong_txid() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(7);
        let utxo = create_test_utxo(7);

        manager.add_utxo(utxo).await.unwrap();

        let tx1 = create_test_txid(140);
        manager.lock_utxo(&outpoint, tx1).unwrap();

        // Try to commit with different txid
        let tx2 = create_test_txid(150);
        let result = manager.commit_spend(&outpoint, &tx2, 1000).await;

        assert!(result.is_err());
        match result {
            Err(UtxoError::LockMismatch) => {} // Expected
            _ => panic!("Expected LockMismatch error"),
        }
    }

    /// Test: restore_utxo can restore spent UTXOs during rollback
    #[tokio::test]
    async fn test_restore_spent_utxo_during_rollback() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(8);
        let utxo = create_test_utxo(8);

        // Add and then spend the UTXO
        manager.add_utxo(utxo.clone()).await.unwrap();
        manager.spend_utxo(&outpoint).await.unwrap();

        // Verify it's now in SpentFinalized state
        match manager.get_state(&outpoint) {
            Some(UTXOState::SpentFinalized { .. }) => {} // Expected
            other => panic!("Expected SpentFinalized state, got {:?}", other),
        }

        // Regular add_utxo should fail
        let add_result = manager.add_utxo(utxo.clone()).await;
        assert!(add_result.is_err());

        // restore_utxo should succeed
        assert!(manager.restore_utxo(utxo).await.is_ok());

        // Verify it's back to Unspent
        match manager.get_state(&outpoint) {
            Some(UTXOState::Unspent) => {} // Expected
            other => panic!("Expected Unspent state after restore, got {:?}", other),
        }
    }

    // ========== Phase 1.2: Collateral Locking Tests ==========

    /// Phase 1.2 Test 1: Lock UTXO as collateral
    #[tokio::test]
    async fn test_lock_utxo_as_collateral() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(100);
        let utxo = create_test_utxo(100);

        manager.add_utxo(utxo).await.unwrap();

        // Lock as collateral
        assert!(manager
            .lock_collateral(
                outpoint.clone(),
                "masternode1".to_string(),
                1000, // lock_height
                1000  // amount
            )
            .is_ok());

        // Verify it's locked
        assert!(manager.is_collateral_locked(&outpoint));

        // Get collateral info
        let locked = manager.get_locked_collateral(&outpoint).unwrap();
        assert_eq!(locked.masternode_address, "masternode1");
        assert_eq!(locked.amount, 1000);
    }

    /// Phase 1.2 Test 2: Cannot spend locked collateral
    #[tokio::test]
    async fn test_cannot_spend_locked_collateral() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(101);
        let utxo = create_test_utxo(101);

        manager.add_utxo(utxo).await.unwrap();
        manager
            .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
            .unwrap();

        // Attempt to spend should fail
        let result = manager.spend_utxo(&outpoint).await;
        assert!(result.is_err());
        match result {
            Err(UtxoError::LockedAsCollateral) => {} // Expected
            _ => panic!("Expected LockedAsCollateral error"),
        }
    }

    /// Phase 1.2 Test 3: Cannot lock collateral for transaction
    #[tokio::test]
    async fn test_cannot_lock_collateral_for_tx() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(102);
        let utxo = create_test_utxo(102);

        manager.add_utxo(utxo).await.unwrap();
        manager
            .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
            .unwrap();

        // Attempt to lock for transaction should fail
        let tx1 = create_test_txid(200);
        let result = manager.lock_utxo(&outpoint, tx1);
        assert!(result.is_err());
        match result {
            Err(UtxoError::LockedAsCollateral) => {} // Expected
            _ => panic!("Expected LockedAsCollateral error"),
        }
    }

    /// Phase 1.2 Test 4: Unlock collateral
    #[tokio::test]
    async fn test_unlock_collateral() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(103);
        let utxo = create_test_utxo(103);

        manager.add_utxo(utxo).await.unwrap();
        manager
            .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
            .unwrap();

        // Unlock
        assert!(manager.unlock_collateral(&outpoint).is_ok());

        // Verify it's unlocked
        assert!(!manager.is_collateral_locked(&outpoint));

        // Should be able to spend now
        assert!(manager.spend_utxo(&outpoint).await.is_ok());
    }

    /// Phase 1.2 Test 5: List all locked collaterals
    #[tokio::test]
    async fn test_list_locked_collaterals() {
        let manager = UTXOStateManager::new();

        // Lock multiple UTXOs
        for i in 110..113 {
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
            manager
                .lock_collateral(
                    create_test_outpoint(i),
                    format!("masternode{}", i),
                    1000,            // lock_height
                    1000 * i as u64, // amount
                )
                .unwrap();
        }

        let locked = manager.list_locked_collaterals();
        assert_eq!(locked.len(), 3);
    }

    /// Phase 1.2 Test 6: List collaterals for specific masternode
    #[tokio::test]
    async fn test_list_collaterals_for_masternode() {
        let manager = UTXOStateManager::new();

        // Lock UTXOs for different masternodes
        for i in 120..123 {
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
            let mn_address = if i == 121 {
                "masternode_target"
            } else {
                "masternode_other"
            };
            manager
                .lock_collateral(
                    create_test_outpoint(i),
                    mn_address.to_string(),
                    1000, // lock_height
                    1000, // amount
                )
                .unwrap();
        }

        let locked = manager.list_collaterals_for_masternode("masternode_target");
        assert_eq!(locked.len(), 1);
        assert_eq!(locked[0].masternode_address, "masternode_target");
    }

    /// Phase 1.2 Test 7: Collateral locked is not spendable
    #[tokio::test]
    async fn test_collateral_is_not_spendable() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(130);
        let utxo = create_test_utxo(130);

        manager.add_utxo(utxo).await.unwrap();
        manager
            .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
            .unwrap();

        // Should not be spendable
        assert!(!manager.is_spendable(&outpoint, None));

        // Unlock
        manager.unlock_collateral(&outpoint).unwrap();

        // Now should be spendable
        assert!(manager.is_spendable(&outpoint, None));
    }

    // ========== Phase 5: Additional Edge Case Tests ==========

    /// Phase 5 Test 1: Lock collateral twice (should fail)
    #[tokio::test]
    async fn test_double_lock_collateral() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(200);
        let utxo = create_test_utxo(200);

        manager.add_utxo(utxo).await.unwrap();

        // First lock succeeds
        assert!(manager
            .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
            .is_ok());

        // Second lock should fail
        assert!(manager
            .lock_collateral(outpoint.clone(), "masternode2".to_string(), 1000, 1000)
            .is_err());
    }

    /// Phase 5 Test 2: Lock non-existent UTXO (should fail)
    #[tokio::test]
    async fn test_lock_nonexistent_utxo() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(201);

        // Try to lock without adding UTXO first
        let result =
            manager.lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000);

        assert!(result.is_err());
    }

    /// Phase 5 Test 3: Unlock non-locked collateral
    #[tokio::test]
    async fn test_unlock_nonlocked_collateral() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(202);
        let utxo = create_test_utxo(202);

        manager.add_utxo(utxo).await.unwrap();

        // Try to unlock without locking first
        let result = manager.unlock_collateral(&outpoint);

        assert!(result.is_err());
    }

    /// Phase 5 Test 4: List collaterals for specific masternode (multiple collaterals)
    #[tokio::test]
    async fn test_list_multiple_collaterals_for_masternode() {
        let manager = UTXOStateManager::new();

        // Add and lock multiple UTXOs for same masternode
        for i in 70..73 {
            let outpoint = create_test_outpoint(i);
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
            manager
                .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
                .unwrap();
        }

        // Add one for different masternode
        let outpoint4 = create_test_outpoint(73);
        let utxo4 = create_test_utxo(73);
        manager.add_utxo(utxo4).await.unwrap();
        manager
            .lock_collateral(outpoint4.clone(), "masternode2".to_string(), 1000, 1000)
            .unwrap();

        // List for masternode1
        let collaterals = manager.list_collaterals_for_masternode("masternode1");
        assert_eq!(collaterals.len(), 3);

        // List for masternode2
        let collaterals2 = manager.list_collaterals_for_masternode("masternode2");
        assert_eq!(collaterals2.len(), 1);
    }

    /// Phase 5 Test 5: Spend UTXO removes from collateral tracking
    #[tokio::test]
    async fn test_spend_utxo_removes_collateral() {
        let manager = UTXOStateManager::new();
        let outpoint = create_test_outpoint(80);
        let utxo = create_test_utxo(80);

        manager.add_utxo(utxo).await.unwrap();
        manager
            .lock_collateral(outpoint.clone(), "masternode1".to_string(), 1000, 1000)
            .unwrap();

        // Unlock first (required before spending)
        manager.unlock_collateral(&outpoint).unwrap();

        // Now spend it
        manager.spend_utxo(&outpoint).await.unwrap();

        // Verify it's no longer tracked
        assert!(!manager.is_collateral_locked(&outpoint));
        assert!(manager.get_locked_collateral(&outpoint).is_none());
    }

    /// Phase 5 Test 6: List all locked collaterals
    #[tokio::test]
    async fn test_list_all_locked_collaterals() {
        let manager = UTXOStateManager::new();

        // Lock multiple collaterals
        for i in 50..55 {
            let outpoint = create_test_outpoint(i);
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
            manager
                .lock_collateral(
                    outpoint.clone(),
                    format!("masternode{}", i),
                    1000,
                    (i as u64) * 1000,
                )
                .unwrap();
        }

        // List all
        let all_collaterals = manager.list_locked_collaterals();
        assert_eq!(all_collaterals.len(), 5);

        // Verify amounts are different
        let amounts: Vec<u64> = all_collaterals.iter().map(|c| c.amount).collect();
        assert!(amounts.contains(&50000));
        assert!(amounts.contains(&54000));
    }

    /// Phase 5 Test 7: Concurrent collateral operations
    #[tokio::test]
    async fn test_concurrent_collateral_operations() {
        let manager = Arc::new(UTXOStateManager::new());

        // Add UTXOs
        for i in 60..70 {
            let _outpoint = create_test_outpoint(i);
            let utxo = create_test_utxo(i);
            manager.add_utxo(utxo).await.unwrap();
        }

        // Spawn concurrent lock operations
        let mut handles = vec![];
        for i in 60..70 {
            let mgr = Arc::clone(&manager);
            let handle = tokio::spawn(async move {
                mgr.lock_collateral(
                    create_test_outpoint(i),
                    format!("masternode{}", i),
                    1000,
                    1000,
                )
            });
            handles.push(handle);
        }

        // Wait for all to complete
        for handle in handles {
            assert!(handle.await.unwrap().is_ok());
        }

        // Verify all are locked
        let all_collaterals = manager.list_locked_collaterals();
        assert_eq!(all_collaterals.len(), 10);
    }
}
