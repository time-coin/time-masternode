# DETAILED CODE ANALYSIS: Locked Funds Bug Investigation

## SECTION 1: CRITICAL DATA STRUCTURES

### LockedCollateral Struct (src/types.rs:487-529)
\\\ust
pub struct LockedCollateral {
    pub outpoint: OutPoint,              // Identifies the UTXO
    pub masternode_address: String,      // Which masternode locked it
    pub lock_height: u64,                // Block height when locked (⚠️ can vary per node)
    pub locked_at: u64,                  // Timestamp
    pub unlock_height: Option<u64>,      // Optional unlock height
    pub amount: u64,                     // Amount locked (⚠️ must match UTXO value)
}
\\\

### UTXOStateManager (src/utxo_manager.rs:44-82)
\\\ust
pub struct UTXOStateManager {
    pub utxo_states: DashMap<OutPoint, UTXOState>,           // Transaction locks
    pub locked_collaterals: DashMap<OutPoint, LockedCollateral>,  // Collateral locks
    collateral_db: Option<sled::Tree>,   // Persistent collateral locks (sled tree)
}

impl UTXOStateManager {
    pub fn enable_collateral_persistence(&mut self, db: &sled::Db) -> Result<(), UtxoError> {
        let tree = db.open_tree("collateral_locks")
            .map_err(|e| UtxoError::Storage(e.into()))?;
        self.collateral_db = Some(tree);
        Ok(())
    }
}
\\\

---

## SECTION 2: THE CRITICAL BUG #2 - UTXO STATE DEPENDENCY

### LOAD PERSISTED (src/utxo_manager.rs:96-128)
\\\ust
pub fn load_persisted_collateral_locks(&self) -> usize {
    let tree = match &self.collateral_db {
        Some(t) => t,
        None => return 0,
    };
    let mut loaded = 0;
    for (key, value) in tree.iter().flatten() {
        if let Ok(locked) = bincode::deserialize::<LockedCollateral>(&value) {
            let outpoint = locked.outpoint.clone();
            
            // ⚠️ CRITICAL: Only restore if UTXO is in Unspent state
            match self.utxo_states.get(&outpoint) {
                Some(state) if matches!(state.value(), UTXOState::Unspent) => {
                    self.locked_collaterals.insert(outpoint, locked);
                    loaded += 1;
                }
                _ => {
                    // ⚠️ BUG: If UTXO is in ANY other state, DELETE from disk
                    let _ = tree.remove(key);
                    tracing::warn!(
                        "🗑️ Removed stale persisted collateral lock (UTXO no longer unspent)"
                    );
                }
            }
        }
    }
    loaded
}
\\\

### REBUILD LOCKS (src/utxo_manager.rs:768-810)
\\\ust
pub fn rebuild_collateral_locks(
    &self,
    entries: Vec<(OutPoint, String, u64, u64)>,
) -> usize {
    let mut restored = 0;
    for (outpoint, masternode_address, lock_height, amount) in entries {
        if self.locked_collaterals.contains_key(&outpoint) {
            continue;  // Skip if already locked
        }
        
        // ⚠️ CRITICAL: Same state check as load_persisted
        match self.utxo_states.get(&outpoint) {
            Some(state) if matches!(state.value(), UTXOState::Unspent) => {
                let locked = LockedCollateral::new(
                    outpoint.clone(),
                    masternode_address,
                    lock_height,
                    amount,
                );
                self.locked_collaterals.insert(outpoint, locked);
                restored += 1;
            }
            _ => {
                tracing::warn!(
                    "⚠️ Cannot restore collateral lock for {:?} — UTXO not in Unspent state",
                    outpoint
                );
            }
        }
    }
    restored
}
\\\

### WHY THIS IS CRITICAL:

During blockchain sync, UTXOs transition through states:
1. Created: Unspent
2. During processing: Locked (transaction lock)
3. After finalization: SpentFinalized or Archived

The problem sequence:
- restart node
- initialize_states() sets all UTXOs to Unspent ✓
- sync starts, blocks being processed
- Collateral UTXO gets locked for transaction: Locked state ← Problem!
- During this window, if node restarts again:
  - load_persisted_collateral_locks() sees Locked state
  - DELETES the lock from disk
  - DELETES from memory
  - Logs warning about "stale" lock
- Result: Collateral lock permanently lost

---

## SECTION 3: LOCK/UNLOCK BUG #1 - PERSISTENCE FAILURES SILENT

### LOCK COLLATERAL (src/utxo_manager.rs:663-709)
\\\ust
pub fn lock_collateral(
    &self,
    outpoint: OutPoint,
    masternode_address: String,
    lock_height: u64,
    amount: u64,
) -> Result<(), UtxoError> {
    // ... state checks ...

    let locked_collateral = LockedCollateral::new(
        outpoint.clone(),
        masternode_address,
        lock_height,
        amount
    );

    // Insert into memory
    self.locked_collaterals
        .insert(outpoint.clone(), locked_collateral.clone());

    // ⚠️ BUG: Persist to disk, but ignore errors
    if let Some(tree) = &self.collateral_db {
        let key = bincode::serialize(&outpoint).unwrap_or_default();
        let value = bincode::serialize(&locked_collateral).unwrap_or_default();
        if let Err(e) = tree.insert(key, value) {
            tracing::warn!("⚠️ Failed to persist collateral lock to disk: {}", e);
            // ⚠️ BUG: Function returns Ok(()) anyway!
        }
    }

    Ok(())  // ← Returns success even if disk write failed
}
\\\

### UNLOCK COLLATERAL (src/utxo_manager.rs:712-731)
\\\ust
pub fn unlock_collateral(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
    if let Some((_, locked)) = self.locked_collaterals.remove(outpoint) {
        // Remove from persistent storage
        if let Some(tree) = &self.collateral_db {
            let key = bincode::serialize(outpoint).unwrap_or_default();
            if let Err(e) = tree.remove(key) {
                tracing::warn!("⚠️ Failed to remove collateral lock from disk: {}", e);
                // ⚠️ BUG: Function still returns Ok(())
            }
        }
        Ok(())  // ← Returns success even if disk write failed
    } else {
        Err(UtxoError::NotFound)
    }
}
\\\

### SCENARIO:
1. Node has SSD failing
2. Lock attempt: in-memory succeeds, disk fails
3. Restart: load_persisted only finds partial locks
4. Report shows less locked funds than expected

---

## SECTION 4: BALANCE REPORTING (CORRECT - NO DOUBLE COUNTING)

### GET_BALANCE (src/rpc/handler.rs:952-998)
\\\ust
async fn get_balance(&self, params: &[Value]) -> Result<Value, RpcError> {
    let utxos = self.utxo_manager.list_utxos_by_address(&filter_addr).await;

    let mut spendable: u64 = 0;
    let mut locked_collateral: u64 = 0;
    let mut pending: u64 = 0;

    for u in &utxos {
        // ✅ CORRECT: Check collateral first
        if self.utxo_manager.is_collateral_locked(&u.outpoint) {
            locked_collateral += u.value;
            continue;  // ← SKIP state check, avoid double-counting
        }
        
        // ✅ CORRECT: Only check state if NOT collateral
        match self.utxo_manager.get_state(&u.outpoint) {
            Some(UTXOState::Unspent) => spendable += u.value,
            Some(UTXOState::Locked { .. }) => pending += u.value,
            Some(UTXOState::SpentPending { .. }) => {},
            Some(UTXOState::SpentFinalized { .. }) => {},
            Some(UTXOState::Archived { .. }) => {},
            None => {},
        }
    }

    let total = spendable + locked_collateral + pending;

    Ok(json!({
        "balance": total as f64 / 100_000_000.0,
        "locked": locked_collateral as f64 / 100_000_000.0,
        "available": spendable as f64 / 100_000_000.0
    }))
}
\\\

✅ **No double-counting**: If locked as collateral, state is not checked (continue statement)

---

## SECTION 5: AUTO-LOCKING BUG #4 - WRONG BLOCK HEIGHT

### CHECK_COLLATERAL_VALIDITY (src/masternode_registry.rs:1202-1267)
\\\ust
pub async fn check_collateral_validity(
    &self,
    masternode_address: &str,
    utxo_manager: &UTXOStateManager,
) -> bool {
    let masternodes = self.masternodes.read().await;
    if let Some(info) = masternodes.get(masternode_address) {
        if let Some(collateral_outpoint) = &info.masternode.collateral_outpoint {
            if utxo_manager.get_utxo(collateral_outpoint).await.is_err() {
                return false;  // UTXO doesn't exist
            }

            // ⚠️ BUG: If not locked, auto-lock with current height
            if !utxo_manager.is_collateral_locked(collateral_outpoint) {
                let lock_height = self.current_height.load(Ordering::Relaxed);
                // ⚠️ This uses current_height, which varies per node
                match utxo_manager.lock_collateral(
                    collateral_outpoint.clone(),
                    masternode_address.to_string(),
                    lock_height,              // ← Different per node!
                    info.masternode.tier.collateral(),
                ) {
                    Ok(()) => {
                        tracing::info!(
                            "🔒 Auto-locked collateral for masternode {}",
                            masternode_address
                        );
                        return true;
                    }
                    Err(e) => {
                        tracing::warn!("⚠️ Could not lock collateral: {:?}", e);
                        return false;
                    }
                }
            }
            return true;
        }
        true  // Legacy masternode without collateral
    } else {
        false
    }
}
\\\

### PROBLEM:
- Called by cleanup tasks periodically
- Node A sees lock_height = 1234
- Node B sees lock_height = 1235
- Same UTXO has inconsistent metadata across network

---

## SECTION 6: STARTUP SEQUENCE (src/main.rs)

### INITIALIZATION PHASE (Line 675-688)
\\\ust
tracing::info!("🔧 Initializing UTXO state manager from storage...");
if let Err(e) = utxo_mgr.initialize_states().await {
    eprintln!("⚠️ Warning: Failed to initialize UTXO states: {}", e);
}

// Load persisted collateral locks from disk
let loaded_locks = utxo_mgr.load_persisted_collateral_locks();
if loaded_locks > 0 {
    tracing::info!(
        "✅ Restored {} collateral lock(s) from persistent storage",
        loaded_locks
    );
}
\\\

Step 1: initialize_states()
- Sets all UTXOs to Unspent state
- This allows load_persisted to restore locks

Step 2: load_persisted_collateral_locks()
- ⚠️ BUGGY: Checks UTXO state (but just set to Unspent, so OK at this point)
- Problem: If any block processing happens between steps, state changes

### REBUILD PHASE - MASTERNODE (Lines 1106-1130)
\\\ust
if let Some(ref mn) = masternode_info {
    // ... register local masternode ...
    
    // Rebuild collateral locks for ALL known masternodes
    {
        let all_masternodes = registry.list_all().await;
        let lock_height = blockchain.get_height();
        let entries: Vec<_> = all_masternodes
            .iter()
            .filter(|info| info.masternode.address != mn.address)
            .filter_map(|info| {
                info.masternode.collateral_outpoint.as_ref().map(|op| {
                    (
                        op.clone(),
                        info.masternode.address.clone(),
                        lock_height,
                        info.masternode.tier.collateral(),
                    )
                })
            })
            .collect();
        if !entries.is_empty() {
            consensus_engine
                .utxo_manager
                .rebuild_collateral_locks(entries);  // ⚠️ Same state check!
        }
    }
}
\\\

---

## SECTION 7: MINIMAL REPRODUCTION TEST

\\\ust
#[tokio::test]
async fn test_collateral_lost_on_restart_during_sync() {
    let manager = UTXOStateManager::new();
    
    // Step 1: Add UTXO to storage
    let utxo = UTXO {
        outpoint: test_outpoint(),
        value: 1_000 * 100_000_000,  // Bronze
        address: "test".into(),
        script_pubkey: vec![],
    };
    manager.storage.insert_utxo(utxo).await;
    manager.initialize_states().await;  // Sets to Unspent
    
    // Step 2: Lock collateral
    manager.lock_collateral(
        test_outpoint(),
        "test_mn".into(),
        100,
        1_000 * 100_000_000,
    ).unwrap();
    
    // Step 3: Simulate UTXO getting locked for transaction processing
    let outpoint = test_outpoint();
    manager.utxo_states.insert(
        outpoint.clone(),
        UTXOState::Locked { 
            txid: Hash256::default(),
            locked_at: current_timestamp(),
        },
    );
    
    // Step 4: Call load_persisted (simulating restart)
    let reloaded = manager.load_persisted_collateral_locks();
    
    // ❌ BUG: reloaded == 0 (should be 1)
    // The lock was DELETED because UTXO wasn't Unspent
    assert_eq!(reloaded, 0);  // ← This shows the bug!
}
\\\

---

## SUMMARY TABLE: Severity & Impact

| Bug | Severity | File:Lines | Impact | Users Affected |
|-----|----------|-----------|--------|-----------------|
| #1 | HIGH | utxo_manager.rs:698,717 | Locks lost if disk fails | Anyone with I/O issues |
| #2 | CRITICAL | utxo_manager.rs:106,778 | Locks deleted during sync | **All nodes during restart** |
| #3 | MEDIUM | utxo_manager.rs:774 | Silent incomplete recovery | Developers debugging |
| #4 | MEDIUM | masternode_registry.rs:1228 | Inconsistent metadata | Network discrepancy |
| #5 | MEDIUM | masternode_registry.rs:1309 | Orphaned locks | Deregistering nodes |
| #6 | MEDIUM | utxo_manager.rs:668 | Wrong amounts stored | Reporting accuracy |
| #7 | MEDIUM | utxo_manager.rs:668 | Invalid tiers locked | System consistency |
| #8 | LOW | utxo_manager.rs:96 | Timing race | Rare cases |

---
