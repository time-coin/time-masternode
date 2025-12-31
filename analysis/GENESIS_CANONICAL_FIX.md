# Genesis Block Consensus Fix

## Problem

All testnet nodes were stuck at height 0 with **different genesis blocks**, causing them to reject each other's blocks:

```
INFO 📥 [Outbound] Received 1 blocks (height 0-0) from [peer] (our height: 0)
WARN ⚠️ [Outbound] All 1 blocks skipped from [peer]
```

Each node independently created its own genesis block with a different hash, leading to network divergence from the start.

## Root Cause

The nodes were **dynamically generating genesis blocks** based on their local view of active masternodes. Even though the code tried to be deterministic by sorting masternodes, timing differences in masternode discovery caused nodes to have different masternode sets when creating genesis, resulting in different genesis hashes.

## Solution

**Load a canonical genesis block from disk** (`genesis.testnet.json`) that contains a pre-computed genesis block with a fixed set of masternodes. This ensures **all nodes use the exact same genesis block hash**.

### Changes Made

1. **Created `genesis.testnet.json`** with canonical genesis block:
   - Fixed masternode set: 50.28.104.50, 64.91.241.10, 69.167.168.176, 165.84.215.117
   - Equal distribution: 25 TIME per masternode (100 TIME total block reward)
   - Genesis timestamp: 1764547200 (2025-12-01T00:00:00Z)

2. **Added `GenesisBlock::load_from_file()`** function:
   - Loads and parses the genesis block from JSON file
   - Searches multiple standard locations
   - Validates and returns the loaded block

3. **Modified `Blockchain::create_genesis_block()`**:
   - First tries to load from file (canonical)
   - Falls back to dynamic generation only if file not found
   - Logs clearly which method was used

## Deployment Instructions

1. **Stop all testnet nodes**:
   ```bash
   sudo systemctl stop timed
   ```

2. **Clear the blockchain database** (IMPORTANT - removes old diverged genesis):
   ```bash
   rm -rf ~/.timecoin/testnet/db
   ```

3. **Update to latest code**:
   ```bash
   cd ~/timecoin
   git pull
   cargo build --release
   sudo cp target/release/timed /usr/local/bin/
   ```

4. **Verify genesis file exists**:
   ```bash
   ls -l ~/timecoin/genesis.testnet.json
   ```
   Should show the canonical genesis file.

5. **Start nodes**:
   ```bash
   sudo systemctl start timed
   ```

6. **Verify all nodes have same genesis**:
   ```bash
   sudo journalctl -u timed -f | grep -i genesis
   ```
   
   Should see:
   ```
   ✓ Loaded genesis block from genesis.testnet.json
   Hash: [same hash on ALL nodes]
   Masternodes: 4
   ```

## Verification

After deployment, all nodes should:
- Load the same genesis block from file
- Show identical genesis hash in logs
- Successfully sync blocks from each other
- Progress beyond height 0

Check consensus:
```bash
# On each node, verify genesis hash matches
curl -s http://localhost:24101/blockchain/status | jq
```

All nodes should report the same `genesis_hash` value.

## Expected Behavior

**Before Fix:**
- Each node at height 0 with different genesis
- Blocks rejected: "All blocks skipped"
- Network stuck, no progress

**After Fix:**
- All nodes load same genesis from file
- Identical genesis hash across network
- Blocks accepted and synced
- Network progresses normally

## File Location Priority

The code searches for `genesis.testnet.json` in:
1. Current directory (`.`)
2. Current directory explicit (`./`)
3. System config (`/etc/timecoin/`)
4. User home (`~/.timecoin/`)

Recommendation: Keep it in the git repository root so it's always available when running from `~/timecoin/`.

## Technical Details

**Genesis Block Hash:**
The canonical genesis block will have a deterministic hash based on:
- Fixed masternode set (sorted by IP)
- Fixed timestamp: 1764547200
- Fixed rewards: 2,500,000,000 satoshis each (25 TIME)
- Coinbase transaction with empty inputs/outputs

**Future Updates:**
If you need to change the genesis block (e.g., for mainnet or testnet relaunch):
1. Edit `genesis.testnet.json` with new values
2. Ensure all nodes have the updated file
3. Clear databases and restart all nodes simultaneously

## Troubleshooting

**Problem:** Node still generates genesis dynamically
- **Check:** Is `genesis.testnet.json` present in working directory?
- **Fix:** Copy file to `/etc/timecoin/genesis.testnet.json` or run from repo root

**Problem:** Nodes still have different genesis
- **Check:** Did all nodes clear their databases?
- **Fix:** `rm -rf ~/.timecoin/testnet/db` and restart

**Problem:** Genesis file parse error
- **Check:** File JSON syntax is valid
- **Fix:** Re-download from git repository

## Commit

Commit: `3f6e070`
Files changed:
- `genesis.testnet.json` - Canonical genesis block
- `src/block/genesis.rs` - Added `load_from_file()` function
- `src/blockchain.rs` - Modified to load from file first
