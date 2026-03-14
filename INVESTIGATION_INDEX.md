# Investigation Report Index

This investigation examined masternode collateral locking in the time-masternode codebase and found **8 critical bugs** causing locked fund reporting issues.

## Report Files Generated

### 1. COLLATERAL_BUGS_SUMMARY.md (Quick Reference)
- **Purpose**: Executive summary with all 8 bugs at a glance
- **Content**: Bug descriptions, severity levels, file locations, quick fix priorities
- **Read Time**: 5 minutes
- **Best For**: Getting overview, understanding impact, prioritizing fixes

### 2. LOCKED_FUNDS_INVESTIGATION.md (Comprehensive Analysis)
- **Purpose**: Full investigation with data structures, functions, potential issues
- **Content**: Complete code review, function analysis, data flow, consistency issues
- **Read Time**: 30 minutes
- **Best For**: Deep understanding, code review, comprehensive documentation

### 3. DETAILED_CODE_ANALYSIS.md (Code-Level Deep Dive)
- **Purpose**: Line-by-line code analysis with bug reproduction
- **Content**: Exact code snippets, bug scenarios, startup sequence, test case
- **Read Time**: 20 minutes
- **Best For**: Developers fixing bugs, understanding root causes, testing fixes

---

## Key Findings

### CRITICAL BUG #2: UTXO State Dependency (MUST FIX FIRST)
**File**: src/utxo_manager.rs lines 106-107, 778-779
**Impact**: Collateral locks are DELETED from disk and memory during blockchain sync
**How It Happens**:
1. Node restart → UTXO states set to Unspent
2. Blockchain syncing → Collateral UTXO locked for transaction processing
3. load_persisted_collateral_locks() sees non-Unspent state → DELETES lock
4. Result: Locked funds appear unlocked

### HIGH BUG #1: Silent Disk Failures
**File**: src/utxo_manager.rs lines 698-700, 717-719
**Impact**: Locks don't persist if disk I/O fails
**Result**: Lost on restart

### MEDIUM BUG #4: Auto-Locking Wrong Height
**File**: src/masternode_registry.rs lines 1228-1236
**Impact**: Different nodes have different lock_height for same UTXO
**Result**: Inconsistent network state

---

## Code Locations Summary

| File | Function | Lines | Bugs |
|------|----------|-------|------|
| src/utxo_manager.rs | lock_collateral | 663-709 | #1, #6, #7 |
| src/utxo_manager.rs | unlock_collateral | 712-731 | #1 |
| src/utxo_manager.rs | load_persisted_collateral_locks | 96-128 | #2, #8 |
| src/utxo_manager.rs | rebuild_collateral_locks | 768-810 | #2, #3 |
| src/masternode_registry.rs | check_collateral_validity | 1202-1267 | #4 |
| src/masternode_registry.rs | cleanup_invalid_collaterals | 1271-1320 | #5 |
| src/rpc/handler.rs | get_balance | 952-998 | ✓ CORRECT |
| src/rpc/handler.rs | get_balances | 1002-1073 | ✓ CORRECT |

---

## What Was NOT A Bug

### Balance Reporting (CORRECT)
- ✅ get_balance() - Correctly separates collateral from other UTXO states
- ✅ get_balances() - Same correct logic for multiple addresses  
- ✅ No double-counting of locked UTXOs
- ✅ Collateral UTXOs excluded from spendable/pending counts

### Basic Lock Management (MOSTLY CORRECT)
- ✅ is_collateral_locked() - Simple and correct
- ✅ get_locked_collateral() - Simple and correct
- ✅ list_locked_collaterals() - Correct iteration
- ✅ list_collaterals_for_masternode() - Correct filtering

### Collateral Validation (CORRECT)
- ✅ alidate_collateral() - Properly validates tier amounts and UTXO spendability

---

## How to Use These Reports

### For Management/Decision Makers
1. Read COLLATERAL_BUGS_SUMMARY.md
2. Focus on the severity levels and impact statements
3. Use table to understand file locations

### For Developers
1. Start with COLLATERAL_BUGS_SUMMARY.md for overview
2. Read DETAILED_CODE_ANALYSIS.md for exact code and reproduction cases
3. Reference LOCKED_FUNDS_INVESTIGATION.md for comprehensive understanding
4. Use file:line numbers to navigate source code

### For QA/Testing
1. Use DETAILED_CODE_ANALYSIS.md "Minimal Reproduction Test" section
2. Reference test cases in LOCKED_FUNDS_INVESTIGATION.md
3. Follow "Testing Checklist" in LOCKED_FUNDS_INVESTIGATION.md

### For Code Review
1. Review DETAILED_CODE_ANALYSIS.md for exact buggy code sections
2. Compare against source files using line numbers
3. Verify fixes against suggested remediation in reports

---

## Fix Priority Order

### Priority 1 (CRITICAL - Fix Immediately)
1. **BUG #2** - Remove UTXO state dependency in load/rebuild functions
   - src/utxo_manager.rs lines 106-107, 778-779
   - This is causing the most reported issue

2. **BUG #1** - Make disk persistence mandatory
   - src/utxo_manager.rs lines 698-700, 717-719
   - Prevent silent data loss

### Priority 2 (HIGH - Fix Soon)
3. **BUG #6** - Validate amount matches UTXO
   - src/utxo_manager.rs line 668
   
4. **BUG #7** - Validate tier collateral amounts
   - src/utxo_manager.rs line 668

5. **BUG #4** - Disable auto-locking
   - src/masternode_registry.rs lines 1224-1256

6. **BUG #5** - Handle unlock errors
   - src/masternode_registry.rs line 1309

### Priority 3 (MEDIUM - Improvements)
7. **BUG #3** - Return status from rebuild
   - src/utxo_manager.rs line 771

8. **BUG #8** - Atomic startup sequence
   - src/main.rs startup section

---

## Investigation Methodology

This investigation:
1. ✅ Examined all files mentioned in scope
2. ✅ Traced data structures through lifecycle
3. ✅ Analyzed startup/restart sequences  
4. ✅ Checked for double-counting in reports
5. ✅ Identified persistence issues
6. ✅ Found race conditions
7. ✅ Located state machine bugs
8. ✅ Verified balance reporting accuracy

Files analyzed:
- src/types.rs - Data structures
- src/utxo_manager.rs - Core UTXO and collateral management
- src/masternode_registry.rs - Masternode and collateral validation
- src/rpc/handler.rs - RPC reporting functions
- src/main.rs - Startup sequence
- src/bin/time-cli.rs - CLI display
- src/bin/time-dashboard.rs - Dashboard display
- src/network/message_handler.rs - Network sync

---

## Most Likely Root Cause

The reported issue of "masternodes reporting wrong locked fund information" is most likely caused by **BUG #2** (CRITICAL severity):

During node restart or blockchain sync, collateral locks are deleted from disk when UTXOs transition to non-Unspent states. This causes locked funds to appear unlocked after restart, especially if the node is restarted during active block processing.

---
