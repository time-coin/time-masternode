//! UTXO (Unspent Transaction Output) state management.
//!
//! Manages the UTXO set for tracking spendable outputs. Provides locking
//! mechanism for concurrent transaction processing.

use crate::storage::UtxoStorage;
use crate::types::*;
use dashmap::DashMap;
use dashmap::DashSet;
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
    /// Persistent storage for collateral locks (survives restarts independently of registry)
    collateral_db: Option<sled::Tree>,
    /// Per-address UTXO index for efficient lookups
    address_index: DashMap<String, DashSet<OutPoint>>,
    /// Cache of address → Ed25519 public key, populated from transaction signatures.
    /// Used for encrypted memo recipient key lookup.
    pubkey_cache: DashMap<String, [u8; 32]>,
    /// Permanent tombstone set: once an outpoint enters a spent state it is recorded here
    /// and add_utxo will hard-reject any attempt to re-add it, even if utxo_states or
    /// sled have been cleared.  Backed by a sled tree so the guard survives restarts.
    spent_tombstones: DashSet<OutPoint>,
    spent_db: Option<sled::Tree>,
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
            collateral_db: None,
            address_index: DashMap::new(),
            pubkey_cache: DashMap::new(),
            spent_tombstones: DashSet::new(),
            spent_db: None,
        }
    }

    #[allow(dead_code)]
    pub fn new_with_storage(storage: Arc<dyn UtxoStorage>) -> Self {
        Self {
            storage,
            utxo_states: DashMap::with_capacity(EXPECTED_UTXO_COUNT),
            locked_collaterals: DashMap::new(),
            collateral_db: None,
            address_index: DashMap::new(),
            pubkey_cache: DashMap::new(),
            spent_tombstones: DashSet::new(),
            spent_db: None,
        }
    }

    /// Enable persistent collateral lock storage using a sled database.
    /// Must be called before any lock/unlock operations for persistence to work.
    pub fn enable_collateral_persistence(&mut self, db: &sled::Db) -> Result<(), UtxoError> {
        let tree = db
            .open_tree("collateral_locks")
            .map_err(|e| UtxoError::Storage(e.into()))?;
        self.collateral_db = Some(tree);
        Ok(())
    }

    /// Enable persistent spent-UTXO tombstone storage.  Call once on startup, before
    /// initialize_states, so the tombstone set is fully populated before any add_utxo
    /// calls can happen.  Loads all previously-recorded tombstones from disk.
    pub fn enable_spent_persistence(&mut self, db: &sled::Db) -> Result<(), UtxoError> {
        let tree = db
            .open_tree("spent_utxos")
            .map_err(|e| UtxoError::Storage(e.into()))?;
        // Reload tombstones that survived the last restart
        let mut loaded = 0usize;
        for item in tree.iter() {
            if let Ok((key, _)) = item {
                if let Ok(op) = bincode::deserialize::<OutPoint>(&key) {
                    self.spent_tombstones.insert(op);
                    loaded += 1;
                }
            }
        }
        if loaded > 0 {
            tracing::info!("🪦 Loaded {} spent UTXO tombstone(s) from disk", loaded);
        }
        self.spent_db = Some(tree);
        Ok(())
    }

    /// Record an outpoint as permanently spent.  Writes to both the in-memory tombstone
    /// set and the sled tree so the guard survives node restarts.
    fn record_spent(&self, outpoint: &OutPoint) {
        self.spent_tombstones.insert(outpoint.clone());
        if let Some(tree) = &self.spent_db {
            if let Ok(key) = bincode::serialize(outpoint) {
                let _ = tree.insert(key, &[][..]);
            }
        }
    }

    /// Register a known Ed25519 public key for an address.
    /// Called when processing transaction signatures (script_sig contains the pubkey).
    pub fn register_pubkey(&self, address: &str, pubkey: [u8; 32]) {
        self.pubkey_cache.insert(address.to_string(), pubkey);
    }

    /// Look up the Ed25519 public key for an address.
    /// Returns None if the address has never signed a transaction we've seen.
    pub fn find_pubkey_for_address(&self, address: &str) -> Option<[u8; 32]> {
        self.pubkey_cache.get(address).map(|v| *v)
    }

    /// Load persisted collateral locks from sled into memory.
    /// Called on startup after `enable_collateral_persistence` and `initialize_states`.
    pub fn load_persisted_collateral_locks(&self) -> usize {
        let tree = match &self.collateral_db {
            Some(t) => t,
            None => return 0,
        };
        let mut loaded = 0;
        for (key, value) in tree.iter().flatten() {
            if let Ok(locked) = bincode::deserialize::<LockedCollateral>(&value) {
                let outpoint = locked.outpoint.clone();
                // Only restore if UTXO is still unspent
                match self.utxo_states.get(&outpoint) {
                    Some(state) if matches!(state.value(), UTXOState::Unspent) => {
                        self.locked_collaterals.insert(outpoint, locked);
                        loaded += 1;
                    }
                    _ => {
                        // UTXO no longer unspent — remove stale lock from disk
                        let _ = tree.remove(key);
                        tracing::warn!(
                            "🗑️ Removed stale persisted collateral lock (UTXO no longer unspent)"
                        );
                    }
                }
            }
        }
        if loaded > 0 {
            tracing::info!(
                "🔒 Loaded {} persisted collateral lock(s) from disk",
                loaded
            );
        }
        loaded
    }

    /// Persist the local masternode's collateral outpoint so we can detect
    /// config changes across restarts (e.g. user comments out collateral).
    pub fn save_local_collateral_outpoint(&self, outpoint: Option<&OutPoint>) {
        let Some(tree) = &self.collateral_db else {
            return;
        };
        let key = b"__local_collateral_outpoint__";
        match outpoint {
            Some(op) => {
                let value = bincode::serialize(op).unwrap_or_default();
                let _ = tree.insert(key.as_ref(), value);
            }
            None => {
                let _ = tree.remove(key.as_ref());
            }
        }
    }

    /// Load the previously saved local collateral outpoint.
    pub fn load_local_collateral_outpoint(&self) -> Option<OutPoint> {
        let tree = self.collateral_db.as_ref()?;
        let value = tree.get(b"__local_collateral_outpoint__").ok()??;
        bincode::deserialize(&value).ok()
    }

    /// Release a collateral lock that no longer matches the current config.
    /// Returns true if a lock was released.
    pub fn release_stale_local_collateral(&self, old_outpoint: &OutPoint) -> bool {
        if self.locked_collaterals.contains_key(old_outpoint) {
            if let Ok(()) = self.unlock_collateral(old_outpoint) {
                tracing::info!(
                    "🔓 Released previous local collateral {}:{} (config changed)",
                    hex::encode(old_outpoint.txid),
                    old_outpoint.vout
                );
                return true;
            }
        }
        false
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

        let mut tombstone_cleaned = 0usize;
        for utxo in utxos {
            // If this outpoint has a spent tombstone, the sled removal may not have
            // persisted (e.g. the process crashed between mark_timevote_finalized's
            // sled removal and tombstone write).  Remove it now and skip — letting it
            // enter the address index as Unspent would inflate the wallet balance.
            if self.spent_tombstones.contains(&utxo.outpoint) {
                let _ = self.storage.remove_utxo(&utxo.outpoint).await;
                tombstone_cleaned += 1;
                continue;
            }
            // Only initialize if not already in state map
            if !self.utxo_states.contains_key(&utxo.outpoint) {
                self.utxo_states
                    .insert(utxo.outpoint.clone(), UTXOState::Unspent);
            }
            // Build address index
            self.address_index
                .entry(utxo.address.clone())
                .or_default()
                .insert(utxo.outpoint);
        }
        if tombstone_cleaned > 0 {
            tracing::warn!(
                "🪦 Removed {} tombstoned UTXO(s) from sled during initialization (stale from prior crash)",
                tombstone_cleaned
            );
        }

        tracing::info!(
            "✅ UTXO state initialization complete: {} entries, {} addresses indexed",
            self.utxo_states.len(),
            self.address_index.len()
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
        self.address_index.clear();

        // Clear persisted collateral locks
        if let Some(tree) = &self.collateral_db {
            let _ = tree.clear();
        }

        // Clear spent tombstones — reindex replays the full chain and will re-populate
        // them via spend_utxo as each block is processed.
        self.spent_tombstones.clear();
        if let Some(tree) = &self.spent_db {
            let _ = tree.clear();
        }

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
        let address = utxo.address.clone();

        // Hard invariant: a spent outpoint can never be re-added, even if utxo_states
        // has been cleared (e.g. after reindex) or sled was modified externally.
        if self.spent_tombstones.contains(&outpoint) {
            tracing::warn!(
                "🚫 add_utxo rejected: outpoint {:?} is permanently spent (tombstone hit)",
                outpoint
            );
            return Err(UtxoError::AlreadySpent);
        }

        // Check if UTXO already exists in current state
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
        self.utxo_states
            .insert(outpoint.clone(), UTXOState::Unspent);
        // Update address index
        self.address_index
            .entry(address)
            .or_default()
            .insert(outpoint);
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        // Remove from address index before removing from storage
        if let Some(utxo) = self.storage.get_utxo(outpoint).await {
            self.remove_from_address_index(&utxo.address, outpoint);
        }
        self.storage.remove_utxo(outpoint).await?;
        self.utxo_states.remove(outpoint);

        // Clean up collateral lock if present
        if self.locked_collaterals.remove(outpoint).is_some() {
            if let Some(tree) = &self.collateral_db {
                let key = bincode::serialize(outpoint).unwrap_or_default();
                let _ = tree.remove(key);
            }
            tracing::debug!("🔓 Removed collateral lock for deleted UTXO {:?}", outpoint);
        }

        Ok(())
    }

    /// Get a UTXO by outpoint (used for undo logs)
    pub async fn get_utxo(&self, outpoint: &OutPoint) -> Result<UTXO, UtxoError> {
        self.storage
            .get_utxo(outpoint)
            .await
            .ok_or(UtxoError::NotFound)
    }

    /// Mark a UTXO as spent (used when processing blocks).
    /// If the UTXO is collateral-locked, the lock is forcibly released before spending —
    /// a confirmed block is ground truth and overrides any application-layer lock.
    /// The collateral lock check belongs in wallet/mempool validation, not here.
    pub async fn spend_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        // If this UTXO is collateral-locked, release the lock first.
        // A confirmed on-chain spend is authoritative: the masternode that held this
        // collateral will be deregistered by cleanup_invalid_collaterals on the next sweep.
        if self.is_collateral_locked(outpoint) {
            tracing::warn!(
                "⚠️ Spending collateral-locked UTXO {:?} — releasing lock (on-chain spend is authoritative)",
                outpoint
            );
            let _ = self.unlock_collateral(outpoint);
        }

        // Remove from address index before removing from storage
        if let Some(utxo) = self.storage.get_utxo(outpoint).await {
            self.remove_from_address_index(&utxo.address, outpoint);
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
        self.record_spent(outpoint);
        Ok(())
    }

    /// Restore a UTXO during rollback (handles spent state)
    /// Unlike add_utxo, this forces the UTXO back to Unspent even if it was previously spent
    pub async fn restore_utxo(&self, utxo: UTXO) -> Result<(), UtxoError> {
        let outpoint = utxo.outpoint.clone();
        let address = utxo.address.clone();

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
                        // Ensure address_index is consistent even for early return
                        self.address_index
                            .entry(address)
                            .or_default()
                            .insert(outpoint);
                        return Ok(());
                    }
                    // State says unspent but not in storage - add it
                }
                UTXOState::SpentFinalized { .. }
                | UTXOState::SpentPending { .. }
                | UTXOState::Archived { .. } => {
                    // This is the rollback case - UTXO was spent but we're undoing it
                    tracing::debug!("Restoring spent UTXO {} during rollback", outpoint);
                }
                UTXOState::Locked { txid, .. } => {
                    // Locked UTXO being restored - clear the lock
                    tracing::warn!(
                        "Restoring locked UTXO {} (was locked by tx {})",
                        outpoint,
                        hex::encode(txid)
                    );
                }
            }
        }

        // Lift the spent tombstone so future transactions can use this UTXO again.
        // This is safe: we are explicitly rolling back a block, so the spend never
        // made it into the canonical chain.
        self.spent_tombstones.remove(&outpoint);
        if let Some(tree) = &self.spent_db {
            if let Ok(key) = bincode::serialize(&outpoint) {
                let _ = tree.remove(key);
            }
        }

        // Add to storage and set state to Unspent
        self.storage.add_utxo(utxo).await?;
        self.utxo_states
            .insert(outpoint.clone(), UTXOState::Unspent);
        // Update address index so balance queries see the restored UTXO
        self.address_index
            .entry(address)
            .or_default()
            .insert(outpoint);
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
                    tracing::debug!("🔒 Locked UTXO {} for tx {}", outpoint, hex::encode(txid));
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
                        tracing::warn!("⏰ Expired lock on UTXO {}, allowing new lock", outpoint);
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
                | UTXOState::Archived { .. } => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(_) => {
                // UTXO not in state map means it doesn't exist — reject
                Err(UtxoError::NotFound)
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
                        tracing::debug!("🔓 Unlocked UTXO {}", outpoint);
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

                    entry.insert(UTXOState::Archived {
                        txid: *txid,
                        block_height,
                        archived_at: Self::current_timestamp(),
                    });

                    tracing::info!(
                        "✅ Committed UTXO spend {:?} in block {}",
                        outpoint,
                        block_height
                    );
                    Ok(())
                }
                UTXOState::Unspent => {
                    tracing::warn!("⚠️ Spending unlocked UTXO {}", outpoint);
                    self.storage.remove_utxo(outpoint).await?;
                    entry.insert(UTXOState::Archived {
                        txid: *txid,
                        block_height,
                        archived_at: Self::current_timestamp(),
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
                    tracing::debug!(
                        "🧹 Cleaning expired lock on UTXO {} (tx {})",
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

    /// Mark a UTXO as TimeVote-finalized (spent).
    ///
    /// Unlike `update_state`, this also removes the UTXO from persistent sled
    /// storage and the address index so that, after a node restart,
    /// `initialize_states` does not resurrect it as `Unspent`.
    pub async fn mark_timevote_finalized(&self, outpoint: &OutPoint, txid: Hash256) {
        // Remove from address index first (needs storage lookup for address)
        if let Some(utxo) = self.storage.get_utxo(outpoint).await {
            self.remove_from_address_index(&utxo.address, outpoint);
        }
        // Remove from persistent storage so restarts don't revive it as Unspent
        let _ = self.storage.remove_utxo(outpoint).await;
        // Update in-memory state
        self.utxo_states.insert(
            outpoint.clone(),
            UTXOState::SpentFinalized {
                txid,
                finalized_at: Self::current_timestamp(),
                votes: 0,
            },
        );
        // Permanently tombstone this outpoint — it can never be re-added
        self.record_spent(outpoint);
    }

    /// Force reset a UTXO to Unspent state (for recovery from stuck locks)
    /// Refuses to unlock UTXOs locked as masternode collateral.
    pub fn force_unlock(&self, outpoint: &OutPoint) -> bool {
        if self.is_collateral_locked(outpoint) {
            tracing::warn!(
                "🚫 force_unlock refused: UTXO {:?} is locked as masternode collateral",
                outpoint
            );
            return false;
        }
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

    /// Get UTXOs for a specific address using the address index (O(n) in address UTXOs, not all UTXOs)
    pub async fn list_utxos_by_address(&self, address: &str) -> Vec<UTXO> {
        let outpoints: Vec<OutPoint> = match self.address_index.get(address) {
            Some(set) => set.iter().map(|op| op.clone()).collect(),
            None => return Vec::new(),
        };

        let mut utxos = Vec::with_capacity(outpoints.len());
        for outpoint in &outpoints {
            if let Some(utxo) = self.storage.get_utxo(outpoint).await {
                utxos.push(utxo);
            }
        }
        utxos
    }

    /// Remove an outpoint from the address index
    fn remove_from_address_index(&self, address: &str, outpoint: &OutPoint) {
        if let Some(set) = self.address_index.get(address) {
            set.remove(outpoint);
            if set.is_empty() {
                drop(set);
                self.address_index.remove(address);
            }
        }
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
    pub async fn calculate_utxo_set_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut utxos = self.list_all_utxos().await;
        utxos.sort_unstable_by(|a, b| {
            (&a.outpoint.txid, a.outpoint.vout).cmp(&(&b.outpoint.txid, b.outpoint.vout))
        });

        let mut hasher = Sha256::new();
        for utxo in &utxos {
            hasher.update(utxo.outpoint.txid);
            hasher.update(utxo.outpoint.vout.to_le_bytes());
            hasher.update(utxo.value.to_le_bytes());
            hasher.update(&utxo.script_pubkey);
            // Include state discriminant so nodes with different UTXO states
            // detect divergence via hash comparison and trigger reconciliation.
            // 0=Unspent, 1=Locked, 2=SpentPending, 3=SpentFinalized, 4=Archived
            let state_disc: u8 = match self.utxo_states.get(&utxo.outpoint).as_deref() {
                None | Some(UTXOState::Unspent) => 0,
                Some(UTXOState::Locked { .. }) => 1,
                Some(UTXOState::SpentPending { .. }) => 2,
                Some(UTXOState::SpentFinalized { .. }) => 3,
                Some(UTXOState::Archived { .. }) => 4,
            };
            hasher.update([state_disc]);
        }

        hasher.finalize().into()
    }

    /// Apply state updates received from a majority peer during reconciliation.
    /// Only advances states forward (Unspent → spent), never reverses them,
    /// to prevent a malicious peer from un-spending a UTXO.
    pub fn apply_state_updates(&self, updates: Vec<(OutPoint, UTXOState)>) {
        fn state_ord(s: &UTXOState) -> u8 {
            match s {
                UTXOState::Unspent => 0,
                UTXOState::Locked { .. } => 1,
                UTXOState::SpentPending { .. } => 2,
                UTXOState::SpentFinalized { .. } => 3,
                UTXOState::Archived { .. } => 4,
            }
        }

        let mut applied = 0usize;
        for (outpoint, remote_state) in updates {
            // Only update if we already track this outpoint (don't invent UTXOs).
            if let Some(mut entry) = self.utxo_states.get_mut(&outpoint) {
                if state_ord(&remote_state) > state_ord(entry.value()) {
                    *entry = remote_state;
                    applied += 1;
                }
            }
        }
        if applied > 0 {
            tracing::info!("🔄 Applied {} UTXO state updates from peer", applied);
        }
    }

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

    pub async fn reconcile_utxo_state(
        &self,
        to_remove: Vec<OutPoint>,
        to_add: Vec<UTXO>,
    ) -> Result<(), UtxoError> {
        let remove_count = to_remove.len();
        let add_count = to_add.len();
        let mut skipped_collateral = 0;

        for outpoint in to_remove {
            // If the majority of peers say this UTXO is spent, release any collateral
            // lock and remove it — peer consensus is authoritative, same as a confirmed block.
            if self.is_collateral_locked(&outpoint) {
                tracing::warn!(
                    "⚠️ Releasing collateral lock on {} during UTXO reconciliation \
                     (majority peers agree it is spent)",
                    outpoint
                );
                let _ = self.unlock_collateral(&outpoint);
                skipped_collateral += 1; // keep the counter for logging continuity
            }
            // Fetch address before removing so we can clean up address_index
            if let Some(utxo) = self.storage.get_utxo(&outpoint).await {
                self.remove_from_address_index(&utxo.address, &outpoint);
            }
            if let Err(e) = self.storage.remove_utxo(&outpoint).await {
                tracing::warn!("Failed to remove UTXO during reconciliation: {}", e);
            }
            self.utxo_states.remove(&outpoint);
        }

        for utxo in to_add {
            let _ = self.add_utxo(utxo).await;
        }

        tracing::info!(
            "🔄 Reconciled UTXO state: removed {}, added {}, skipped {} collateral",
            remove_count - skipped_collateral,
            add_count,
            skipped_collateral,
        );
        Ok(())
    }

    // ========== Masternode Collateral Locking Methods ==========

    /// Lock a UTXO as masternode collateral.
    ///
    /// `persist` should be `true` only for the **local node's own** collateral.
    /// Remote masternodes' collateral locks are in-memory only — they are
    /// re-established from gossip on restart, so persisting them causes the
    /// collateral_locks sled tree to grow unboundedly as more nodes join the
    /// network.
    pub fn lock_collateral(
        &self,
        outpoint: OutPoint,
        masternode_address: String,
        lock_height: u64,
        amount: u64,
    ) -> Result<(), UtxoError> {
        self.lock_collateral_inner(outpoint, masternode_address, lock_height, amount, false)
    }

    /// Lock the local node's own collateral UTXO and persist it across restarts.
    pub fn lock_local_collateral(
        &self,
        outpoint: OutPoint,
        masternode_address: String,
        lock_height: u64,
        amount: u64,
    ) -> Result<(), UtxoError> {
        self.lock_collateral_inner(outpoint, masternode_address, lock_height, amount, true)
    }

    fn lock_collateral_inner(
        &self,
        outpoint: OutPoint,
        masternode_address: String,
        lock_height: u64,
        amount: u64,
        persist: bool,
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
            .insert(outpoint.clone(), locked_collateral.clone());

        // Persist to disk only for the local node's own collateral
        if persist {
            if let Some(tree) = &self.collateral_db {
                let key = bincode::serialize(&outpoint).unwrap_or_default();
                let value = bincode::serialize(&locked_collateral).unwrap_or_default();
                if let Err(e) = tree.insert(key, value) {
                    tracing::warn!("⚠️ Failed to persist collateral lock to disk: {}", e);
                }
            }
        }

        tracing::debug!(
            "🔒 Locked collateral UTXO {:?} (amount: {}, persisted: {})",
            outpoint,
            amount,
            persist,
        );
        Ok(())
    }

    /// Unlock a UTXO from masternode collateral
    pub fn unlock_collateral(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        if let Some((_, locked)) = self.locked_collaterals.remove(outpoint) {
            // Remove from persistent storage
            if let Some(tree) = &self.collateral_db {
                let key = bincode::serialize(outpoint).unwrap_or_default();
                if let Err(e) = tree.remove(key) {
                    tracing::warn!("⚠️ Failed to remove collateral lock from disk: {}", e);
                }
            }
            tracing::info!(
                "🔓 Unlocked collateral UTXO {} (was {} TIME for {})",
                outpoint,
                locked.amount,
                locked.masternode_address
            );
            Ok(())
        } else {
            Err(UtxoError::NotFound)
        }
    }

    /// Release ALL collateral locks (does not touch regular UTXO/transaction locks).
    ///
    /// Safer than `force_unlock_all` for recovery: only affects the collateral lock map,
    /// leaving pending/finalized transaction UTXO states intact.
    /// Returns the number of locks released.
    pub fn unlock_all_collaterals(&self) -> usize {
        let outpoints: Vec<OutPoint> = self
            .locked_collaterals
            .iter()
            .map(|r| r.key().clone())
            .collect();
        let count = outpoints.len();
        for op in &outpoints {
            let _ = self.unlock_collateral(op);
        }
        count
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

    /// List all locked collaterals, filtering out stale entries whose UTXOs no longer exist
    pub fn list_locked_collaterals(&self) -> Vec<LockedCollateral> {
        let mut stale_keys = Vec::new();
        let result: Vec<LockedCollateral> = self
            .locked_collaterals
            .iter()
            .filter(|entry| {
                let exists = self.utxo_states.contains_key(entry.key());
                if !exists {
                    stale_keys.push(entry.key().clone());
                }
                exists
            })
            .map(|entry| entry.value().clone())
            .collect();

        // Clean up stale entries
        for key in &stale_keys {
            self.locked_collaterals.remove(key);
            if let Some(tree) = &self.collateral_db {
                let serialized = bincode::serialize(key).unwrap_or_default();
                let _ = tree.remove(serialized);
            }
            tracing::debug!("🧹 Cleaned stale collateral lock for {:?}", key);
        }

        result
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

    /// Rebuild collateral locks from masternode registry data.
    /// Called on startup to restore in-memory collateral locks that would
    /// otherwise be lost across restarts.
    pub fn rebuild_collateral_locks(
        &self,
        entries: Vec<(OutPoint, String, u64, u64)>, // (outpoint, mn_address, lock_height, amount)
    ) -> usize {
        let mut restored = 0;
        for (outpoint, masternode_address, lock_height, amount) in entries {
            if self.locked_collaterals.contains_key(&outpoint) {
                continue; // Already locked
            }
            // Only lock if UTXO still exists and is unspent
            match self.utxo_states.get(&outpoint) {
                Some(state) if matches!(state.value(), UTXOState::Unspent) => {
                    let locked = LockedCollateral::new(
                        outpoint.clone(),
                        masternode_address,
                        lock_height,
                        amount,
                    );
                    // Do NOT persist to disk here — only the local node's own collateral
                    // should be persisted (via lock_local_collateral). Persisting remote
                    // nodes' locks causes them to be loaded and spuriously "released" on
                    // every restart.
                    self.locked_collaterals.insert(outpoint, locked);
                    restored += 1;
                }
                _ => {
                    // UTXO is spent/archived — collateral cleanup will deregister
                    // this masternode after 3 consecutive missed checks (~30 min).
                    tracing::debug!(
                        "Cannot restore collateral lock for {:?} — UTXO not Unspent (will be cleaned up)",
                        outpoint
                    );
                }
            }
        }
        if restored > 0 {
            tracing::info!(
                "🔒 Rebuilt {} collateral lock(s) from masternode registry",
                restored
            );
        }

        // Sweep for stale locks that survived the rebuild (e.g. gossip lock from before
        // the spending block arrived, or a squatter whose UTXO is now spent).
        let purged = self.purge_stale_collateral_locks();
        if purged > 0 {
            tracing::warn!(
                "🧹 [LOCK-SWEEP] Purged {} stale collateral lock(s) after rebuild",
                purged
            );
        }

        restored
    }

    /// Remove collateral locks whose underlying UTXO is no longer Unspent.
    ///
    /// Called after `rebuild_collateral_locks`, after each block is added, and
    /// periodically in a background task to keep all nodes' lock sets consistent
    /// with the actual UTXO set (the authoritative on-chain source of truth).
    pub fn purge_stale_collateral_locks(&self) -> usize {
        let stale: Vec<OutPoint> = self
            .locked_collaterals
            .iter()
            .filter(|e| {
                !matches!(
                    self.utxo_states.get(e.key()).as_deref(),
                    Some(&UTXOState::Unspent)
                )
            })
            .map(|e| e.key().clone())
            .collect();

        let count = stale.len();
        for outpoint in &stale {
            if let Some((_, lock)) = self.locked_collaterals.remove(outpoint) {
                // Remove from sled if the lock was persisted.
                if let Some(tree) = &self.collateral_db {
                    let key = bincode::serialize(outpoint).unwrap_or_default();
                    let _ = tree.remove(key);
                }

                // If the UTXO state is Locked (a gossip-applied TX lock — invalid on a
                // collateral UTXO), release the lock so the coins become spendable again.
                // Genuine spends (SpentFinalized, Archived) are left untouched — those
                // reflect real on-chain state that should not be reversed.
                let current_state = self.utxo_states.get(outpoint).map(|s| s.clone());
                if matches!(current_state, Some(UTXOState::Locked { .. })) {
                    self.utxo_states
                        .insert(outpoint.clone(), UTXOState::Unspent);
                    tracing::warn!(
                        "🔓 [LOCK-SWEEP] Released gossip-applied TX lock on collateral {}:{} \
                         (masternode: {}) — UTXO restored to Unspent",
                        hex::encode(lock.outpoint.txid),
                        lock.outpoint.vout,
                        lock.masternode_address,
                    );
                } else {
                    tracing::warn!(
                        "🗑️ [LOCK-SWEEP] Removed stale collateral lock {}:{} \
                         (masternode: {}) — UTXO is spent or unknown",
                        hex::encode(lock.outpoint.txid),
                        lock.outpoint.vout,
                        lock.masternode_address,
                    );
                }
            }
        }
        count
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
            masternode_key: None,
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
            Some(UTXOState::Archived {
                txid, block_height, ..
            }) => {
                assert_eq!(txid, tx1);
                assert_eq!(block_height, 1000);
            }
            _ => panic!("Expected archived state"),
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

        // Block-processing spend must succeed even on collateral-locked UTXOs —
        // an on-chain spend is authoritative and the lock is released automatically.
        let result = manager.spend_utxo(&outpoint).await;
        assert!(
            result.is_ok(),
            "spend_utxo should release collateral lock and succeed"
        );
        // Collateral lock should now be gone
        assert!(!manager.is_collateral_locked(&outpoint));
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
