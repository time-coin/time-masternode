# Database Reset Instructions (UPDATED)

## Automatic Schema Migration (NEW)

**Good news:** As of commit 09d5619, nodes now **automatically migrate old-schema blocks** on startup. This means:

✅ You can sync genesis from peers running old code
✅ No more "io error:" crashes from schema mismatches
✅ Database resets are **optional** - migration happens automatically

## How It Works

When a node starts:
1. **Migration check runs first** (before any blockchain operations)
2. Each existing block is checked:
   - If it deserializes with current schema → OK, skip it
   - If it fails → try deserializing as BlockV1 (old format)
   - If BlockV1 works → convert to new format and re-save
3. All migrated blocks are flushed to disk
4. Normal blockchain operations continue

You'll see logs like:
```
🔄 Checking for old-schema blocks that need migration...
✅ Migrated block 0 from old schema
✅ Migrated block 1 from old schema
✅ Schema migration complete: 2 blocks migrated
```

## When to Still Reset Database

Schema migration handles **most cases**, but you should still reset if:

1. **Migration fails repeatedly** - indicates severe corruption
2. **Blocks have invalid data** - migration can't fix logical errors
3. **Want a clean sync** - faster than migrating thousands of blocks
4. **Testing/debugging** - need known-good state

## Problem (Historical Context)

After schema changes (specifically adding `time_attestations` field to Block), nodes with existing databases experienced "io error:" when trying to save new blocks. This was because:

1. Old blocks in database were serialized with the old Block schema
2. New code cannot deserialize them due to schema mismatch
3. Database operations fail, preventing block storage

## Solution Options

### Option 1: Let Automatic Migration Handle It (Recommended)

Just update to latest code and restart:
```bash
cd /path/to/timecoin
git pull
cargo build --release
sudo systemctl restart timed.service
```

The migration runs automatically on startup. Watch logs to verify:
```bash
journalctl -u timed.service -f | grep -i migrat
```

### Option 2: Manual Database Reset (If Migration Fails)


Clear the database on all nodes to remove old-schema blocks. The nodes will then:
1. Sync genesis block from the network (if it exists)
2. Sync all subsequent blocks from peers
3. Store blocks with the new schema

## Steps to Reset Database

### On Linux Nodes (testnet)

```bash
# Stop the node
sudo systemctl stop timed.service

# Clear the blockchain database
rm -rf /root/.timecoin/testnet/db/blocks

# Optional: Clear all databases for a complete fresh start
# rm -rf /root/.timecoin/testnet/db/*

# Start the node
sudo systemctl start timed.service

# Watch the logs to verify sync
journalctl -u timed.service -f
```

### On Mainnet

```bash
# Stop the node
sudo systemctl stop timed.service

# Clear the blockchain database
rm -rf /root/.timecoin/mainnet/db/blocks

# Start the node
sudo systemctl start timed.service

# Watch the logs
journalctl -u timed.service -f
```

## What Happens After Reset

1. **Node starts with empty database**
2. **Genesis sync attempt (new behavior)**:
   - Node waits 10 seconds for peer connections
   - Requests genesis (block 0) from connected peers
   - If genesis exists on network, it syncs from peers
3. **Genesis generation (fallback if no network genesis)**:
   - Waits 45 seconds for masternode discovery
   - Elects leader (lowest masternode address)
   - Leader generates dynamic genesis and broadcasts
   - Followers receive genesis from leader
4. **Block sync**:
   - Node requests missing blocks from peers
   - Syncs to current network height
   - Begins participating in block production

## Expected Log Output

### Successful Genesis Sync from Network
```
🌱 No genesis found locally - attempting to sync from network
📥 Requesting genesis block from 3 connected peer(s)
✅ Successfully synced genesis block from network
```

### Dynamic Genesis Generation (first-time network startup)
```
🌱 No genesis on network - initiating dynamic generation
⏳ Waiting 45 seconds for masternodes to discover each other...
🎲 Genesis leader election: 6 masternodes registered, leader = 69.167.168.176
👑 We are the genesis leader - generating genesis block
📤 Broadcasting genesis block to all peers
```

### Block Sync
```
✅ Initial blockchain sync complete
📥 [Outbound] Received BlocksResponse: 10 blocks from peer
✅ Added block 1 to blockchain
✅ Added block 2 to blockchain
...
```

## Troubleshooting

### "io error:" still occurring after reset
- Verify database was actually deleted: `ls -la /root/.timecoin/testnet/db/`
- Check disk space: `df -h`
- Check file permissions: `ls -l /root/.timecoin/`
- Try clearing ALL databases: `rm -rf /root/.timecoin/testnet/db/*`

### Nodes stuck at height 0 after reset
- Check peer connections: nodes should see each other in logs
- Verify at least one node has blocks to share
- If all nodes are at height 0: restart one leader node first, let it generate genesis, then restart others

### Genesis hash mismatch
- This indicates incompatible blockchains
- All nodes must reset their databases together
- Or: one node keeps its DB, others sync from it

## Prevention

Future schema changes should include database version checks or migration code to avoid this issue.
