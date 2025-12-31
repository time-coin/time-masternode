# Duplicate Connection Issue Analysis

## Problem
Logs show repeated connection attempts to the same peers with messages like:
```
🔄 Rejecting duplicate inbound connection from X.X.X.X (already have outbound)
```

This happens every ~10 seconds, even though the peer check runs every 2 minutes.

## Root Cause

The issue is a **race condition between reconnection tasks and the periodic peer check**:

1. **Reconnection Task**: When a connection fails, `spawn_connection_task` creates a persistent task that retries with exponential backoff (10s, 20s, 40s, 80s, 160s, 300s)

2. **Periodic Peer Check (PHASE3)**: Every 2 minutes, the system checks all known peers and tries to connect to any that aren't connected

3. **The Race**: The periodic check sees a peer as "not connected" (because the reconnection task is in backoff sleep) and spawns a **new** connection task, duplicating the effort

## Current Safeguards

The code already has several safeguards:

```rust
// In periodic peer check (line 301-309)
if connection_manager.is_connected(ip).await {
    continue;
}

// Atomically check and mark as connecting
if !connection_manager.mark_connecting(ip).await {
    // Another task already connecting, skip
    continue;
}
```

```rust
// In reconnection task (line 408-425)
// Check if already connected/connecting before reconnecting
if connection_manager.is_connected(&ip).await {
    break;
}

if !connection_manager.mark_connecting(&ip).await {
    break;
}
```

## Why Safeguards Don't Fully Prevent Duplicates

The `mark_connecting` is **cleared** when a connection attempt fails and enters backoff sleep:
- Line 428: `connection_manager.mark_disconnected(&ip).await;`

This allows the periodic check to see the peer as "available" and start another connection task.

## Solution Options

### Option 1: Track Reconnection Backoff State (Recommended)
Add a "reconnecting" state to `PeerConnectionManager` that persists during backoff:

```rust
pub struct PeerConnectionManager {
    connected: Arc<RwLock<HashSet<String>>>,
    connecting: Arc<RwLock<HashSet<String>>>,
    reconnecting: Arc<RwLock<HashMap<String, (Instant, u64)>>>, // (next_attempt, attempt_count)
}
```

Modify periodic check to skip peers in backoff:
```rust
if connection_manager.is_reconnecting(ip).await {
    continue;
}
```

### Option 2: Increase Peer Check Interval
Change from 2 minutes to 5 minutes to reduce overlap with reconnection attempts.

### Option 3: Centralize All Connection Management
Remove the persistent reconnection tasks entirely. Have the periodic check handle all connection attempts with the existing exponential backoff logic.

## Recommendation

**Implement Option 1** - it provides the cleanest separation between automatic reconnection and new peer discovery, while maintaining the persistent connection tasks that ensure we don't lose masternodes.

## Impact

- **Current**: ~200-300 duplicate connection attempts per hour per peer
- **After Fix**: Near-zero duplicate attempts
- **Performance**: Minimal CPU/network overhead from tracking backoff state
