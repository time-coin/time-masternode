# TimeCoin Architecture Overview

**Last Updated:** 2026-05-15
**Version:** 1.5.1 (Historical log suppression, watchdog systemd migration)

---

## Recent Updates (v1.5.1 — May 15, 2026)

### Historical Block Log Suppression (commit 0f7e816)

Masternode registration and deregistration events logged at `INFO` level during initial chain replay produced thousands of noisy lines when a node first synced the full chain, making it hard to spot genuine operational issues.

**Root cause:** `is_syncing` (the `AtomicBool` flag) is only `true` inside `sync_from_peers()`. Blocks arriving via the peer message handler during initial chain download bypass that path, so `is_syncing` was `false` even while downloading thousands of historical blocks.

**Fix in `src/blockchain.rs`:** Added an `is_historical` local variable at the top of `process_special_transactions()`:

```rust
let is_historical = self.is_syncing.load(Ordering::Acquire)
    || (now_secs().saturating_sub(block.header.timestamp) > 3600);
```

All three `is_syncing.load()` checks (Registration, Deregistration, PayoutUpdate branches) were replaced with `is_historical`. A block timestamped more than 1 hour in the past is definitively historical regardless of which sync path delivered it. Live masternode activity always appears in blocks within the current 600-second slot.

### Watchdog Deployed as systemd Service

The masternode watchdog (`mn-watchdog.sh`) was previously run inside a `screen` session, which provided no automatic restart on crash, no log retention in journald, and no boot survival without custom startup scripts.

**Changes:**

- **`scripts/install-masternode.sh`** (commit `77d81ab`): Added `install_watchdog()` function. On fresh installs, the script now:
  1. Copies `scripts/mn-watchdog.sh` → `/usr/local/bin/mn-watchdog`
  2. Generates a network-aware `/etc/systemd/system/mn-watchdog.service` with `BindsTo=timed.service` (mainnet) or `BindsTo=timetd.service` (testnet)
  3. Runs `systemctl enable --now mn-watchdog`

- **`scripts/migrate-watchdog-to-systemd.sh`** (commit `22b53e9`): One-shot migration script for existing nodes running the watchdog in a `screen` session. Supports `--testnet`, `--dry-run`, and `--help` flags. Steps:
  1. Auto-detects network from `time.conf`
  2. Installs watchdog binary and service file
  3. Enables and starts the systemd service
  4. Kills the `screen -S watchdog` session
  5. Verifies `systemctl status mn-watchdog` is active

**To migrate an existing node:**
```bash
# Preview (no changes)
sudo bash scripts/migrate-watchdog-to-systemd.sh --dry-run

# Apply
sudo bash scripts/migrate-watchdog-to-systemd.sh

# Check status
systemctl status mn-watchdog
journalctl -u mn-watchdog -f
```

---

## Recent Bug Fixes and Features (May 2026)

### Signed MasternodeUnlock — Gossip-Based Collateral Release (commit 8e13246)

Deregistering a masternode no longer requires spending the collateral UTXO
on-chain. Inspired by Dash's `ProUpRevTx`, the daemon now broadcasts a signed
`MasternodeUnlock` gossip message when a collateral line is removed from
`masternode.conf` and the daemon restarts.

**How it works:**

1. On startup, if a stale outpoint is detected (config changed), the daemon
   records the old `OutPoint → masternode_address` mapping before releasing the
   local lock.
2. 15 seconds after startup (once peers have connected), it broadcasts:
   ```
   MasternodeUnlock {
     address:             String,       // masternode wallet address
     collateral_outpoint: OutPoint,
     timestamp:           u64,
     signature:           Vec<u8>,      // #[serde(default)] for backward compat
   }
   ```
3. The `signature` is an Ed25519 signature over the canonical proof string
   `"TIME_COLLATERAL_REVOKE:<address>:<txid_hex>:<vout>:<timestamp>"` using
   `masternodeprivkey`.
4. Receiving nodes verify the signature against the stored `public_key` for
   `address`. Valid revokes → unregister + queue UTXO unlock + relay. Unsigned
   revokes are accepted only from a direct (non-relayed) TCP connection whose
   source IP matches the masternode's registered IP.

**Operator workflow:** Comment out the collateral line in `masternode.conf`,
restart `timed`. Within ~15 seconds of peer connections establishing, all nodes
release the lock and the collateral UTXO is freely spendable again.

### Outbound Masternode Announcement Fix (commit d9a4369)

Previously, when a node dialed OUT to a peer, it never sent a masternode
announcement — only inbound connections triggered one. Peers that the local node
dialled first therefore did not know the connecting node was a masternode, causing
registry gaps and missed reward eligibility signals.

**Fix in `src/network/peer_connection.rs`:** Outbound connections now send a
`MasternodeAnnouncementV4` (or V3 for older peers) immediately after completing
the handshake, mirroring the existing behaviour in `server.rs` for inbound
connections.

### Collateral-Churn Guard: Tier Upgrades Allowed (commit 31ac714)

The Collateral-Churn guard (Case A) in `src/masternode_registry.rs` previously
blocked **all** outpoint replacements when the old UTXO still existed on-chain,
preventing legitimate tier upgrades (e.g. Silver → Gold). The guard now allows
the replacement when `prefetched_utxo_addr == masternode.wallet_address`,
confirming that the new UTXO is owned by the same operator. Unauthenticated
replacements by third parties remain blocked. The log message changes from
`[Collateral-Churn] Blocked...` to `[Collateral-Upgrade] Allowed...` for
legitimate upgrades.

### Startup Log: Wallet Address Display Fix (commit d9a4369)

`src/main.rs` (lines ~1049 and ~1076) previously logged `mn.address` (the node's
IP address) where the masternode's TIME wallet address should appear at startup.
Fixed to display `mn.wallet_address` (the TIME address receiving rewards).

---

## Recent Updates (v1.5.x - April 28, 2026)

### ConnectionManager as Single Authority

All connection state — inbound and outbound — is now exclusively tracked in `ConnectionManager`. Previously, inbound connections were registered only in `PeerConnectionRegistry`'s writer-channel map, so `ConnectionManager.is_connected()` returned `false` for inbound peers. PHASE3 in `client.rs` would then see the peer as "not connected" and dial it again, creating a collision storm.

**Changes:**
- `accept_inbound(peer_ip, is_whitelisted) -> bool` added to `ConnectionManager`: atomically registers an inbound connection, rejects any existing state (Connecting/Connected/Reconnecting), and increments `inbound_count`. Returns `false` (drop the connection) if the peer already exists in any state or inbound capacity is at limit.
- `server.rs` calls `connection_manager.accept_inbound()` at the top of `handle_peer()`, before any message work. Duplicate inbounds are silently dropped.
- PHASE3 in `client.rs` checks `connection_manager.is_connected()` (covers both directions) instead of `peer_registry.is_connected()` (outbound-only).
- All connection-count and AV25 subnet checks in `client.rs` use only `ConnectionManager`.
- `PeerConnectionRegistry` remains the writer-channel router (peer_ip → PeerWriterTx) but is no longer an authoritative connection state source.

### Conflict-Only TimeVote Voting

The consensus engine no longer runs 67% stake-weighted TimeVote for every transaction. It auto-finalizes immediately unless another mempool transaction is spending the same UTXOs.

**Before:** Every transaction required vote accumulation — could take seconds or stall entirely on a degraded network, causing transactions to sit in the mempool indefinitely.

**After:** Only genuine double-spend conflicts trigger a full TimeVote round. Transactions with no competing spender are finalized as soon as they pass basic validation.

**Implementation:** `TransactionPool::has_conflicting_transaction(inputs, exclude_txid) -> bool` was added. `ConsensusEngine::submit_transaction()` calls it before initiating TimeVote; with no conflict (or <3 active validators, or dev mode) the transaction auto-finalizes immediately.

### Avalanche-Style Transaction Gossip

Transaction gossip now fires **before** local processing. Previously, `TransactionBroadcast` was relayed only inside the `Ok(_)` arm of `process_transaction()`. If a node rejected a transaction locally (e.g., UTXO not yet synced), propagation stopped at that node — causing partition-dependent tx visibility.

**Fix in `server.rs`:**
- Structural validity check (0 inputs / 0 outputs) gates gossip.
- `broadcast_tx.send()` fires before `process_transaction()`.
- Nodes that are syncing still relay but skip local processing, so transactions propagate through the entire network even during sync.

### UTXO State Sync Fix (Locked/SpentPending)

Mid-block UTXO reconciliation (300s hash comparison → divergence → full set diff → state sync) was querying only `Unspent` UTXOs when requesting state updates from peers. UTXOs in `Locked` or `SpentPending` state — representing transactions in flight when connectivity broke — were invisible and never recovered.

**Fix in `message_handler.rs`:** The post-reconciliation UTXO filter now includes `Unspent | Locked { .. } | SpentPending { .. }`, ensuring all active non-archived UTXOs are included in state sync requests.

### Connection Collision Fix

When two nodes dial each other simultaneously, one connection must be dropped. `priority_reconnect_notify` previously fired unconditionally on any disconnect, causing reconnect storms where both sides immediately re-dialed the dropped collision connection.

**Fix in `masternode_registry.rs`:** `priority_reconnect_notify` now fires only when the disconnected session lasted ≥ 10 seconds (genuine unintended disconnect). Collision-dropped connections (very short lifetime) exit quietly without triggering a re-dial.

### Transaction Persistence and Reliability

- **Finalized TX persistence** (`d2c02b8`): finalized transactions are written to sled and restored on startup so they survive restarts and can be included in the next block.
- **Pending TX silent expiry fix** (`eefd54d`): transactions with `approved` or `pending` status are never evicted due to age — only removed via block inclusion.
- **Approved/pending eviction protection** (`e85605f`): fee-based pool eviction also skips transactions that have reached approval status.
- **Whitelisted peers never banned** (`cf32e44`): normal violation accumulation is now skipped for whitelisted masternodes.
- **`rebroadcasttransaction <txid>` RPC/CLI** (`9f99a5a`): looks up a pending or finalized transaction and rebroadcasts it to all peers — useful for rescuing transactions stuck due to missed propagation.

---

## Recent Updates (v1.5.0 - April 26, 2026)

### Spent UTXO Tombstone — Hard Re-Add Prevention

Added a permanent spent-outpoint tombstone to `UTXOStateManager`. Once any UTXO enters a spent state (`mark_timevote_finalized` or `spend_utxo`), its outpoint is written to:

- **In-memory `spent_tombstones: DashSet<OutPoint>`** — checked first in `add_utxo`, before touching `utxo_states` or sled storage.
- **Sled `spent_utxos` tree** — persisted to disk so the guard survives node restarts.

`add_utxo` now hard-rejects any call for a tombstoned outpoint with `UtxoError::AlreadySpent`, even if `utxo_states` has been cleared (e.g., after a reindex or future cleanup). This closes a class of bugs and attack vectors where a spent UTXO could silently re-enter the live UTXO set.

`clear_all()` (invoked by reindex) wipes both the in-memory set and the sled tree so full chain replay correctly repopulates tombstones via `spend_utxo`. `restore_utxo()` (used during fork rollbacks) lifts the tombstone for the specific outpoint being un-spent, since the spend is being reversed on the canonical chain.

**Startup sequence:** `enable_spent_persistence()` must be called before `initialize_states()` so all previously-recorded tombstones are loaded before any `add_utxo` calls can occur.

### Peer Node UTXO Double-Count on Restart (Bug Fix)

When a peer node received a `TransactionFinalized` message, the handler in `server.rs` called `update_state(input, SpentFinalized)` — which updated only the in-memory `utxo_states` DashMap. Input UTXOs remained in sled storage and the `address_index`. After a node restart, `initialize_states` reloads from sled and resurrects those inputs as `Unspent`, so both the original inputs AND the new outputs appear as spendable — doubling the reported balance for any wallet querying that node.

**Fix:** Replaced `update_state` with `mark_timevote_finalized` in the `TransactionFinalized` peer handler. This removes inputs from both sled storage and the `address_index`, matching the behaviour of the originating node's auto-finalization path.

### Wallet-GUI Consolidation Balance Inflation (Bug Fix)

WebSocket events (`TransactionReceived`, `UtxoFinalized`) arrive at the wallet before `broadcast_transaction` RPC returns, so the txid is not yet in `consolidation_txids` when those events are processed. With no send-record and `is_consolidation=false`, a phantom receive entry was created for every consolidation output, inflating the displayed balance by 2×.

**Fix (wallet-gui `service.rs`):** Both WS handlers now check `consolidation_active && is_own_addr` as a fallback. If true, the txid is registered into `consolidation_txids` immediately (before RPC returns) and the event is classified as consolidation change, preventing the phantom entry.

---

## Recent Updates (v1.7.0 - April 2026)

### Genesis Verification False Disconnect Fix (Critical)

`handle_genesis_hash_response` had an unconditional `Err("DISCONNECT: genesis hash mismatch ...")` at the end of the function that fired for every peer — including those whose genesis hash **matched** ours. The compatible branch correctly logged "✅ compatible" and reset fork errors, but then fell through to the trailing `Err` which disconnected the peer with a spurious mismatch showing identical hashes on both sides.

**Cascade observed on mainnet (April 6 2026):**
- Every genesis-responding peer was disconnected immediately after passing verification
- `is_genesis_confirmed()` was never set for any peer
- Fork resolution was unconditionally skipped: *"Skipping fork resolution — peer not genesis-confirmed (likely old code)"*
- All nodes stuck at height 753 despite 20+ peers being at 754–757

**Fix:** The compatible branch now calls `mark_genesis_confirmed()` and `return Ok(None)`. The `Err("DISCONNECT: ...")` is only reached when hashes genuinely differ. `mark_genesis_confirmed()` is now `pub` so `message_handler` can call it directly.

**Genesis verification paths (two):**
1. **Background task** (`verify_genesis_compatibility` in `peer_connection_registry.rs`): sends `GetBlockHash(0)`, waits for `BlockHashResponse` — correctly called `mark_genesis_confirmed()` already.
2. **Inline handler** (`handle_genesis_hash_response` in `message_handler.rs`): handles incoming `GenesisHashResponse` messages — this was the broken path; now fixed to also call `mark_genesis_confirmed()` on match.

---

## Recent Updates (v1.6.0 - March 19, 2026)

### Re-Sync Watchdog

- A background task runs every **5 minutes** after initial sync completes and compares the local height against all connected peers' reported chain tips
- If any peer is more than 1 block ahead, `sync_from_peers(target)` is triggered automatically
- Fixes the observed production stall where a node would declare initial sync "complete" after all peers timed out, then sit idle for hours despite being 985 blocks behind

### Collateral Deregistration Grace Period

- `cleanup_invalid_collaterals()` now requires a masternode's collateral UTXO to be missing for **3 consecutive block checks** before deregistration (previously immediate)
- A per-masternode miss counter (`collateral_miss_counts` DashMap) resets immediately if the UTXO reappears
- Prevents split-brain from transient UTXO-set divergence at block boundaries — a masternode that appears deregistered on one node due to timing no longer causes reward-recipient mismatches that cascade into block rejection

### VRF Weight Cap Alignment (Validator ↔ Producer)

- The block validator in `message_handler.rs` now applies the same **Free-tier weight cap** (`Bronze.sampling_weight() - 1 = 9`) as the producer's self-selection code in `main.rs`
- Previously the producer capped Free-tier effective weight at 9 when computing its own VRF threshold, but the validator used the raw weight (1) plus uncapped fairness bonus — causing both sides to compute different thresholds
- The `total_sampling_weight` sum in the validator also now applies the cap, keeping it consistent with what the producer uses

### VRF Relaxation Interval Alignment (Producer ↔ Validator)

- **`LEADER_TIMEOUT_SECS`: 5 → 10** — the producer now advances `leader_attempt` every 10 seconds, matching the validator's `elapsed / 10` relaxation intervals
- Previously paid-tier nodes would broadcast a proposal at 5s when the validator still required 10s elapsed to apply any relaxation, guaranteeing one wasted rejection per stalled slot
- **Free-tier gate raised: `attempt ≥ 3` (15 s) → `attempt ≥ 5` (50 s)** — with capped weight 9 and a ~2338-weight network, a Free-tier node needs multiplier ≥ 32 (2^5, elapsed ≥ 50 s) for the validator to accept its proposal; the old gate at 15 s produced 35 seconds of guaranteed-rejected proposals per stalled slot

---

## Recent Updates (v1.5.0 - March 2026)

### Solo Block Production Prevention

- **`MIN_AGREEING_PEERS = 2`** enforced in both the longest-chain-rule path and the weighted consensus path of `check_2_3_consensus_for_production()`. A node can no longer produce blocks without at least 2 other peers confirming they're on the same chain at the same height.
- **Fallback consensus requires `prepare_weight > 1`**: the TimeGuard fallback no longer accepts the producer's own vote as sufficient. Since the producer always votes for its own block (`prepare_weight >= 1`), this prevented solo block production via timeout.
- **Single-node attack defense**: a malicious node producing a block early to claim a higher chain cannot stall other nodes. Only ≥2 independent peers reporting a plausible height (within 1 block of time-based expected) will block production. A single peer ahead triggers background sync only.

### Block Timing Enforcement

- **Production-side**: `produce_block_at_height()` refuses to create a block before `genesis_timestamp + (height × BLOCK_TIME_SECONDS)`. This prevents nodes from racing ahead of schedule.
- **Validation-side**: `validate_block()` rejects incoming blocks with timestamps before their scheduled time (30s clock-skew grace). Only enforced for recent blocks (within 10 of chain tip) — historical blocks during sync are exempt.

### Sync & Catch-Up Fixes

- **Sync loop no longer blocks catch-up**: when all peers are at the same height and far behind the time-based target, `sync_from_peers` requests fresh chain tips, waits 3s, and re-checks. If no peer is ahead after refresh, allows production immediately instead of entering a 120s sync loop.
- **Consensus failure triggers sync**: the block production loop in `main.rs` now spawns `sync_from_peers` when `check_2_3_consensus` fails, instead of just logging and retrying.
- **Fork alerts update chain tip cache**: `handle_fork_alert` writes the alerting peer's consensus height/hash into the chain tip cache, enabling `sync_from_peers` to discover peers ahead.

### Fork Resolution (Bugs 1-4)

- **Bug 1**: Same-height fork detection blocks production when peers at our height have different hashes
- **Bug 2**: Deterministic hash tiebreaker (lower hash wins) for same-height forks
- **Bug 3**: Non-consensus peer blocking threshold lowered to gap > 10 (was > 20)
- **Bug 4**: `handle_fork` uses small batches (20 blocks) with 60s stall detection; `MAX_BLOCKS_PER_RESPONSE = 50`

### Faster Peer Connections

- Startup delays reduced ~60%: `PEER_WAIT` 15→5s, `GENESIS_WAIT` 20→10s, discovery backoff 30→10s base
- Peer exchange interval 60→30s, health monitoring 120→30s start / 60s interval, PHASE 3 rediscovery 120→30s

---

## Recent Updates (v1.3.0 - March 2026)

### Ed25519 Block Producer Signatures

- **`producer_signature` field added to `BlockHeader`**: the block producer signs the block hash with its Ed25519 key after VRF selection
- **Prevents VRF proof reuse**: without this, a valid VRF proof could be detached from its original block and paired with tampered content (different transactions, rewards, or merkle root)
- **Verified in two places**: `validate_block_before_vote()` for newly proposed blocks; `add_block()` for synced blocks from peers
- **Backward compatible**: empty signatures are accepted for pre-signature blocks; `#[serde(default)]` on the field

### VRF Eligibility: 3-Block Rolling Participation Window

- **Replaced single-block bitmap gate** with a rolling window spanning the last 3 blocks
- A masternode is VRF-eligible if it appeared in the `consensus_participants_bitmap` (or was block producer) in any of the 3 most-recent blocks
- **Motivation**: high-latency nodes whose precommit vote arrived slightly late for one round were excluded from the bitmap and systematically locked out of VRF sortition, losing block-producer rewards
- **Grace period**: a node must miss 3 consecutive rounds before losing VRF eligibility; one late vote no longer disqualifies

### Genesis Checkpoint Enforcement

- **Testnet genesis hash hardcoded** in `constants.rs`; `GenesisBlock::verify_checkpoint()` validates the hash on startup and on every `add_block` at height 0
- **Infinite fork-resolution loop fixed**: previously, a genesis hash mismatch caused endless retry loops as the node attempted to resolve a fork with a peer on a different chain; now a `genesis_mismatch_detected` flag is set after the first mismatch at `common_ancestor=0` and further attempts are suppressed with a logged warning
- **No automatic data deletion**: the operator must manually resolve a genesis mismatch; the node never deletes its own chain based on a peer's claim

### Block Producer Signature Mismatch During Sync (Warning, Not Error)

- **Changed from fatal rejection to logged warning** when a synced block's `producer_signature` fails verification
- **Root cause**: a freshly syncing node has stale public keys in its masternode registry (loaded from disk before the chain is rebuilt); collateral-UTXO checks prevent live announcements from updating those keys until enough UTXOs are synced, creating a dead-lock
- **Safety**: the block hash chain still guarantees integrity; once the node reaches the chain tip the registry is refreshed via live announcements and real-time blocks are fully verified

### Reward Address Routing Fix

- **`masternode re-registration now overwrites `wallet_address`** when `reward_address` in `time.conf` changes
- **Previous bug**: changing `reward_address` and restarting did not update the stored `masternode.wallet_address`; block rewards continued routing to the old local wallet instead of the newly configured GUI wallet address
- **Fix applied in `register_internal()`** in `masternode_registry.rs`

---

## Recent Updates (v1.4.0 - March 2026)

### Full-Mesh Topology for Small Networks

- **`FULL_MESH_THRESHOLD = 50`**: when the total masternode count ≤ 50, Phase 1 of `client.rs` connects every node to every other node regardless of tier, guaranteeing that gossip, TimeVotes, and reward-eligibility sightings reach all participants
- **Pyramid topology still used** when the network exceeds 50 masternodes: Gold → Silver → Bronze → Free hierarchy with reserved upstream-slot allocation as before
- The threshold is evaluated at connection time; as nodes join and the count crosses 50 the topology transitions automatically

### Peer Eviction for Persistently Dead Peers

- **5 consecutive failures** → peer is permanently evicted from the sled `peer_manager` DB and the AI profile in `adaptive_reconnection.rs`
- **Exponential cooldown** before re-attempt: 3 failures = 10 min, 5 = 40 min (max before eviction)
- **Eviction check runs unconditionally** during PHASE 3 slot-filling — checked before AI reconnection advice, not only during AI cooldown periods
- **Gossip re-addition blocked:** `PeerManager` tracks evicted IPs with a 1-hour cooldown; `add_peer_candidate()` rejects recently-evicted addresses even if re-advertised via PeerExchange
- **Startup filtering:** `list_by_tier()` only returns active masternodes — PHASE 1 no longer dials inactive masternodes
- Phase 3-MN (masternode reconnect loop) was removed; masternodes connect outbound themselves on startup

### PeerExchange with Load-Aware Routing

- `GetPeers` responses now return **`PeerExchangeEntry`** structs instead of bare addresses:
  ```rust
  pub struct PeerExchangeEntry {
      pub address: String,
      pub connection_count: u32,
      pub is_masternode: bool,
      pub tier: Option<MasternodeTier>,
  }
  ```
- Entries are sorted by tier (Gold first) then by ascending `connection_count` so new peers prefer underloaded nodes
- **Overload redirect**: a node with `connection_count > 70% of MAX_INBOUND (100)` rejects new inbounds and sends an alternative `PeerExchangeEntry` list so the connecting peer can try a less-loaded peer instead
- `PeerConnectionRegistry` tracks per-peer load via a `peer_load` DashMap

### Block Catch-Up Acceleration

- **`LEADER_TIMEOUT_SECS` reduced from 10 s to 5 s** — offline leaders are skipped twice as fast when the node is syncing
- **Free-tier VRF eligibility** reduced from attempt ≥ 6 (60 s deadlock) to **attempt ≥ 3 (15 s deadlock)**, improving testnet and small-network liveness
- When **`blocks_behind > 50`**, `leader_attempt` starts at 1 (pre-boost) so paid tiers skip the strict first cycle and catch-up proceeds faster

### Masternode Active Status — Three Fixes

- **Direct TCP is authoritative**: `cleanup_stale_reports()` now accepts a `peer_registry` reference and never flips `is_active = false` for a peer that has a live direct connection, regardless of the gossip counter
- **Gossip self-recording**: `broadcast_status_gossip()` now calls `process_status_gossip()` locally before broadcasting so a node records its own sightings in its own registry
- **Dynamic minimum-reports threshold**: 1 (≤ 4 nodes), 2 (5–12 nodes), 3 (13+ nodes) — prevents premature deactivation on small testnets

### Consensus Bitmap Includes Gossip-Active Nodes

- When building `consensus_participants_bitmap` during block production (`src/blockchain.rs`), **direct voters** (TimeVote prepare/precommit) are now **merged with gossip-active masternodes**
- This ensures that pyramid-topology nodes not directly connected to the block producer still appear in the bitmap and remain eligible for tier-pool rewards and VRF leader selection
- **Anti-gaming preserved**: gossip requires ~30–60 s to accumulate; nodes joining mid-block or later than the observation window do not qualify
- **Reward eligibility chain**: gossip or direct-vote → bitmap in block N → rewards in block N+1

### `get_connected_peers()` Returns Post-Handshake Peers Only

- `PeerConnectionRegistry::get_connected_peers()` previously included all entries in the connections DashMap, including peers in `Connecting` state (TCP not yet established)
- Now cross-references the `peer_writers` map — only IPs with a live, non-closed writer channel are returned
- Eliminates the bug where AI peer selection and sync logic operated on not-yet-connected peers

---

## Recent Updates (v1.2.0 - February 22, 2026)

### Fork Resolution Simplification

- **Removed stake override logic** from `fork_resolver.rs`: stake can no longer override the longest chain rule
- **Three simple rules**: (1) reject future timestamps, (2) longer chain always wins, (3) same height uses stake then hash tiebreaker
- **`handle_fork()`** simplified to flat early-return structure (no stake override acceptance path)
- **`check_2_3_consensus_for_production()`** now counts behind-peers as agreeing and includes own weight in total

### VRF Sortition Tightening

- **`TARGET_PROPOSERS` reduced from 3 to 1**: targets exactly one block producer per slot, reducing competing blocks
- **Wall-clock deadlock detection**: VRF threshold relaxation now uses real elapsed time waiting at a height, not time since slot was scheduled (prevents all nodes being eligible during catch-up)
- **Free-tier sybil protection**: Free nodes require 15 s of deadlock (attempt ≥ 3) before receiving VRF boost (reduced from 60 s / attempt ≥ 6 in v1.4.0)

### Catch-up Micro-fork Prevention

- **Non-consensus peer filter relaxed for small gaps**: blocks from peers 1-5 blocks ahead are accepted from any whitelisted peer (consensus list is stale during rapid catch-up)

### Masternode Key System (Dash-style)

- **Replaced certificate-based key system** with single `masternodeprivkey` in `time.conf`
- **`masternode genkey` RPC/CLI command**: generates base58check-encoded Ed25519 private key
- **masternode.conf simplified**: 3-field format (alias, txid, vout) — key and IP are in time.conf, not masternode.conf
- **Certificate system removed**: no more `MASTERNODE_AUTHORITY_PUBKEY`, `verify_masternode_certificate()`, or website registration
- **Backward compatibility**: old 5/6-field masternode.conf formats still parsed (extra fields ignored)

### Previous Updates (v1.1.0 - February 2026)

**Bug #4: Fork Resolution Inconsistency (Feb 1, 2026)**
- **Issue**: VRF tiebreaker used "higher score wins" but hash tiebreaker used "lower hash wins"
- **Impact**: Network fragmentation - nodes on same-height fork couldn't agree on canonical chain
- **Root Cause**: `choose_canonical_chain()` had VRF score comparison that contradicted hash tiebreaker
- **Fix**: Removed VRF score from fork resolution; now uses "lower hash wins" consistently everywhere
- **Result**: All fork resolution paths (blockchain.rs, masternode_authority.rs) now agree using 2/3 weighted stake consensus

**Bug #1: Broadcast Callback Not Wired**
- **Issue**: Consensus engine had no way to broadcast TimeVote requests
- **Impact**: Vote requests never sent to network, transactions never finalized network-wide
- **Root Cause**: `set_broadcast_callback()` method existed but was never called in initialization
- **Fix**: Wired up `peer_connection_registry.broadcast()` as consensus callback in main.rs after network server initialization
- **Result**: TimeVote consensus now fully functional end-to-end

**Bug #2: Finalized Pool Premature Clearing**
- **Issue**: Finalized transaction pool cleared after EVERY block addition
- **Impact**: Locally finalized transactions lost before they could be included in locally produced blocks
- **Root Cause**: `clear_finalized_transactions()` called blindly without checking if TXs were in the block
- **Fix**: Added `clear_finalized_txs(txids)` to selectively clear only transactions actually in the added block
- **Result**: Finalized transactions now properly persist until included in a block

**Bug #3: Hardcoded Version String**
- **Issue**: Version hardcoded as "1.0.0" instead of using Cargo.toml
- **Impact**: Impossible to distinguish nodes with new TimeVote code from old nodes
- **Fix**: Use `env!("CARGO_PKG_VERSION")` compile-time macro
- **Result**: Version now automatically reflects Cargo.toml (currently 1.1.0)

### TimeVote Transaction Flow (Now Working)

```
1. TX Submission → RPC (sendtoaddress)
                ↓
2. Validation → Lock UTXOs (SpentPending state)
                ↓
3. Broadcast → TransactionBroadcast to all peers
                ↓
4. TimeVote Request → Broadcast vote request (NOW WORKING!)
                ↓
5. Vote Collection → Validators respond with signed votes
                ↓
6. Vote Accumulation → Stake-weighted sum calculated
                ↓
7. Finalization → 67% threshold → Move to finalized pool (ALL NODES)
                ↓
8. TimeProof Assembly → Collect Accept votes, create proof
                ↓
9. Block Production → Query finalized pool, include TXs
                ↓
10. Block Addition → Process UTXOs, selectively clear finalized pool (NOW WORKING!)
                ↓
11. Archival → TX confirmed on blockchain
```

---

## System Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      Application Layer                   │
│  ┌──────────────────────────────────────────────────┐   │
│  │  Main Application (main.rs)                      │   │
│  │  - Initialization & Configuration              │   │
│  │  - Graceful Shutdown Manager                   │   │
│  │  - Task Coordination                           │   │
│  └──────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                           │
       ┌───────────────────┼───────────────────┐
       │                   │                   │
       ▼                   ▼                   ▼
┌──────────────────────┐ ┌─────────────────┐ ┌──────────────────┐
│  Consensus           │ │  Network        │ │  Storage         │
│  Engines             │ │  Layer          │ │  Layer           │
│  - TimeVote          │ │  - P2P TCP      │ │  - Sled DB       │
│    (TX Finality)     │ │  - Message Relay│ │  - UTXO Manager  │
│  - TimeLock          │ │  - Peer Mgmt    │ │  - TX Pool       │
│    (Block Producer)  │ │  - Heartbeats   │ │  - Block Chain   │
│  - AI Fork Resolver  │ │  - Fork Sync    │ │  - AI History    │
└──────────────────────┘ └─────────────────┘ └──────────────────┘
       │                   │                   │
       └───────────────────┼───────────────────┘
                           │
                    ┌──────▼──────┐
                    │  Blockchain │
                    │  - Blocks   │
                    │  - Chain    │
                    │  - State    │
                    └─────────────┘
```

---

## Core Components

### 1. Consensus Engine - TimeVote Protocol (`consensus.rs`)

**Responsibility:** Transaction validation, ordering, and finality

**Key Features:**
- **TimeVote Protocol:** Continuous voting consensus with stake-weighted validator voting
- **Progressive TimeProof Assembly:** Signed votes accumulate to form verifiable proof
- **Unified Finality:** Single finality state (67% weight threshold, liveness fallback to 51% after 30s)
- **Instant Finality:** Transactions finalized in ~750ms average
- **UTXO Locking:** Prevents double-spending during consensus
- **Deterministic Finality:** No forks after finality achieved

**Optimizations:**
- ✅ ArcSwap for masternode list (lock-free reads)
- ✅ OnceLock for identity (set-once, read-many)
- ✅ spawn_blocking for signature verification
- ✅ DashMap for transaction state tracking (per-txid)
- ✅ Per-txid consensus isolation (parallel processing)

**Data Structures:**
```rust
pub struct ConsensusEngine {
    timevote: Arc<TimeVoteConsensus>,           // Consensus state
    masternodes: ArcSwap<Vec<Masternode>>,      // Lock-free
    utxo_manager: Arc<UTXOStateManager>,        // UTXO state
    tx_pool: Arc<TransactionPool>,              // Mempool
}

pub struct TimeVoteConsensus {
    tx_state: DashMap<Hash256, Arc<RwLock<VotingState>>>,      // Per-TX state
    active_rounds: DashMap<Hash256, Arc<RwLock<QueryRound>>>, // Vote tracking
    finalized_txs: DashMap<Hash256, Preference>,             // Finalized set
}
```

---

### 2. Block Production - TimeLock (`tsdc.rs`)

**Responsibility:** Deterministic block production and checkpointing

**Key Features:**
- **TimeLock:** Block leader elected per 10-min slot
- **VRF-Based Leader Selection:** Cryptographically verifiable randomness
- **Fixed Block Time:** Blocks produced every 10 minutes (600 seconds)
- **Checkpoint Creation:** Finalizes all pending TimeVote transactions
- **Masternode Rotation:** Fair leader selection based on stake

**Key Insight:**
- TimeLock is **NOT** a consensus algorithm - it's a block production schedule
- Actual consensus for transaction finality happens in TimeVote (seconds)
- TimeLock just bundles already-finalized transactions into periodic blocks

**Optimizations:**
- ✅ VRF prevents leader bias
- ✅ Deterministic output (no randomness after computation)
- ✅ O(1) leader lookup per slot

**Data Structures:**
```rust
pub struct TimeLockConsensus {
    validators: Arc<RwLock<Vec<TimeLockValidator>>>,  // Active validators
    current_slot: AtomicU64,                           // Current time slot
    finalized_height: AtomicU64,                       // Last finalized block
}
```

---

### 2.1 Fork Resolution Rules

**Canonical Chain Selection** (deterministic, all nodes agree):

1. **Longer chain wins** - Higher block height is always canonical
2. **Lower hash wins** - At equal height, lexicographically smaller block hash is canonical

**Consistency:** This rule is applied uniformly across:
- `blockchain.rs` - `compare_chain_with_peers()` (height-first, stake tiebreaker)
- `ai/fork_resolver.rs` - Longest-chain fork decisions (3 simple rules)
- `masternode_authority.rs` - Masternode chain authority analysis
- `network/peer_connection.rs` - Peer chain comparison

**Why lower hash?**
- Deterministic: All nodes compute same result
- Simple: No external dependencies
- Standard: Follows Bitcoin/Ethereum convention
- Verifiable: Anyone can check the comparison

**Note:** VRF is used for **leader selection** (who produces blocks), NOT for fork resolution tiebreaking.

---

### 3. Transaction Pool (`transaction_pool.rs`)

**Responsibility:** Mempool management

**Key Features:**
- Stores pending transactions awaiting consensus
- Enforces size limits (10,000 tx max, 300MB max)
- Evicts lowest-fee transactions when full
- Tracks finalized transactions
- Maintains rejection cache

**Optimizations:**
- ✅ DashMap for lock-free access (no global lock)
- ✅ AtomicUsize for O(1) metrics
- ✅ PoolEntry metadata (fee, size, timestamp)
- ✅ Fee-based eviction policy

**Data Structures:**
```rust
pub struct TransactionPool {
    pending: DashMap<Hash256, PoolEntry>,         // Lock-free pending
    finalized: DashMap<Hash256, PoolEntry>,       // Lock-free finalized
    rejected: DashMap<Hash256, (String, Instant)>,// Rejection cache
    pending_count: AtomicUsize,                   // O(1) counter
    pending_bytes: AtomicUsize,                   // O(1) counter
}

struct PoolEntry {
    tx: Transaction,
    fee: u64,
    added_at: Instant,
    size: usize,
}
```

---

### 4. Storage Layer (`storage.rs`)

**Responsibility:** Persistent data storage

**Key Features:**
- Sled-based key-value store
- Batch operations for atomic writes
- High-throughput mode enabled
- Optimized cache sizing

**Optimizations:**
- ✅ spawn_blocking for all I/O operations
- ✅ Batch operations for atomicity
- ✅ Optimized sysinfo usage
- ✅ Proper error types

**Implementation:**
```rust
pub struct SledUtxoStorage {
    db: sled::Db,
}

impl UtxoStorage for SledUtxoStorage {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        let db = self.db.clone();
        spawn_blocking(move || {
            let key = bincode::serialize(outpoint).ok()?;
            let value = db.get(&key).ok()??;
            bincode::deserialize(&value).ok()
        }).await.ok()?
    }
}
```

---

### 5. UTXO Manager (`utxo_manager.rs`)

**Responsibility:** UTXO state management with consensus integration

**Key Features:**
- Tracks unspent transaction outputs with state machine:
  - **Unspent:** Available for spending
  - **SpentPending:** Input locked during TimeVote consensus
  - **Spent:** Transaction finalized
- Prevents double-spending via state locking
- Calculates UTXO set hash for validation
- State transitions during consensus rounds

**Optimizations:**
- ✅ DashMap for lock-free concurrent access
- ✅ Per-address UTXO index (`DashMap<String, DashSet<OutPoint>>`) for O(n-per-address) lookups
- ✅ Streaming UTXO iteration
- ✅ Efficient hash calculation
- ✅ Entry API for atomic operations
- ✅ Auto-consolidation when transfers need >5000 inputs

**Data Structures:**
```rust
pub struct UTXOStateManager {
    storage: Arc<dyn UtxoStorage>,
    utxo_states: DashMap<OutPoint, UTXOState>,              // Lock-free state
    locked_collaterals: DashMap<OutPoint, LockedCollateral>,
    collateral_db: Option<sled::Tree>,                      // Persisted collateral locks
    address_index: DashMap<String, DashSet<OutPoint>>,      // Per-address UTXO index
    pubkey_cache: DashMap<String, [u8; 32]>,                // Ed25519 pubkey cache
    spent_tombstones: DashSet<OutPoint>,                    // Permanent spent guard (in-memory)
    spent_db: Option<sled::Tree>,                           // Spent tombstones persisted to disk
}
```

---

### 6. Network Layer

**Responsibility:** P2P peer communication with persistent connections

**Key Features:**
- **Full-Mesh Topology (≤ 50 masternodes):** Every node connects to every other node, ensuring all gossip, votes, and reward-eligibility sightings are universally visible on testnet and small networks
- **Pyramid Topology (> 50 masternodes):** Gold/Silver/Bronze/Free hierarchy with reserved upstream-slot allocation for scalability
- **Persistent Masternode Mesh:** Two-way connections established once, never disconnected
- **Message Types:**
  - TransactionBroadcast: New transactions
  - TransactionVoteRequest: TimeVote vote requests
  - TransactionVote: Validator votes for TimeVote
  - UTXOStateUpdate: State changes during consensus
  - BlockProposal: TimeLock block production
  - Heartbeat: Liveness detection
- **Peer Discovery:** Masternode registry queries
- **Handshakes:** Network validation and peer identification
- **Connection Pooling:** Persistent connections per peer

**Connection Design:**
```
Masternode A ←→ Masternode B  (persistent TCP, no disconnect)
      ↓             ↓
Masternode C        
      ↓             ↓
   Full Node ←→ Full Node
```

---

### 7. Main Application (`main.rs`)

**Module Structure:**
```
main.rs
├── error.rs             - Unified error types
└── shutdown.rs          - Graceful shutdown management
```

**Key Features:**
- ✅ Graceful shutdown with CancellationToken
- ✅ Task registration and cleanup
- ✅ Configuration management
- ✅ Comprehensive error handling

**Shutdown Flow:**
```
Ctrl+C Signal
    │
    ▼
ShutdownManager::cancel()
    │
    ├─→ CancellationToken::cancel()
    │
    └─→ All spawned tasks receive signal
            │
            ├─→ Heartbeat task exits
            ├─→ TimeVote consensus exits
            ├─→ TimeLock block production exits
            ├─→ Network loop exits
            │
            ▼
        All await handles completed
            │
            ▼
        Process exits cleanly
```

**Note:** Internal code may reference "Avalanche" for historical reasons - this refers to the TimeVote Protocol implementation.

---

## Data Flow

### Transaction Finality Flow (TimeVote Consensus)

For submitting a single transaction:

```
User submits transaction (RPC sendrawtransaction)
    │
    ▼
ConsensusEngine::submit_transaction()
    ├─→ Validate transaction syntax & inputs
    ├─→ Lock UTXOs (state → SpentPending)
    ├─→ Broadcast to all masternodes
    ├─→ Add to TransactionPool (pending)
    │
    ▼
Initiate TimeVote Consensus (Unified Finality)
    ├─→ Transaction enters "Voting" state
    ├─→ Create QueryRound for vote tracking
    │
    ▼
Execute TimeVote Rounds (progressive TimeProof assembly)
    ├─→ Sample k validators (stake-weighted)
    ├─→ Send TransactionVoteRequest
    ├─→ Collect signed votes for 2 seconds
    ├─→ Accumulate unique signed votes toward TimeProof
    │
    ├─→ If α votes for Accept:
    │   ├─→ Add signed votes to TimeProof
    │   ├─→ Update accumulated weight
    │
    └─→ If accumulated_weight ≥ Q_finality (67% of AVS weight, 51% liveness fallback):
        ├─→ Transaction FINALIZED (single unified state)
        ├─→ TimeProof complete (verifiable by anyone)
        ├─→ Move to finalized pool
        ├─→ Notify clients (instant finality ~750ms)
        │
        ▼
TimeLock Block Production (every 10 minutes)
    ├─→ Collect finalized transactions
    ├─→ Select TimeLock leader via VRF
    ├─→ Bundle into block
    ├─→ Commit to blockchain
    │
    ▼
Transaction in blockchain (permanent checkpoint)
```

For submitting multiple independent transactions in parallel:

```
ConsensusEngine::batch_submit_transactions(Arc<Self>, Vec<Transaction>)
    │
    ├─→ partition_non_conflicting(txs)
    │       Groups transactions with disjoint UTXO inputs into independent sets.
    │       Transactions sharing an input (double-spends) land in separate groups
    │       and are rejected at the UTXO lock step.
    │
    └─→ tokio::spawn one task per transaction (all concurrent)
            Each task calls submit_transaction() independently.
            Total finality time = ~750ms (one round), not N × 750ms.
```

`batch_submit_transactions` is a non-consensus optimization: the 67% finality threshold, `TimeProof` structure, and on-chain serialization are all unchanged. It is safe to call from any async context via `Arc<ConsensusEngine>`.

**TimeVote Parameters:**
- **Sample size (k):** 20 validators per round
- **Quorum (α):** 14 responses needed for decision
- **Finality threshold (Q_finality):** 67% of AVS weight (falls back to 51% after 30s stall for liveness)
- **Query timeout:** 2 seconds per round
- **Typical finality:** 750ms (varies with network)

---

### Block Production Flow (TimeLock)

```
Slot Timer (every 10 minutes)
    │
    ▼
TimeLock::select_leader()
    ├─→ Calculate VRF output for current slot
    ├─→ Determine leader (deterministic)
    │
    ▼
If local node is leader:
    ├─→ Collect all finalized transactions
    ├─→ Generate deterministic block
    ├─→ Sign block
    ├─→ Broadcast BlockProposal
    │
    ▼
All nodes receive block
    ├─→ Validate block signature
    ├─→ Verify all transactions are finalized
    ├─→ Apply block to blockchain
    ├─→ Update UTXO state (SpentPending → Spent)
    │
    ▼
Block committed (immutable checkpoint)
    ├─→ TimeVote-finalized transactions now blockchain-confirmed
    ├─→ Clients can rely on finality
```

**TimeLock Parameters:**
- **Block time:** 10 minutes (600 seconds)
- **Leader selection:** VRF-based (deterministic, cannot be gamed)
- **Transactions included:** Only those finalized by TimeVote
- **Block finality:** Permanent (cannot be reverted)

---

## Concurrency Model

### Lock Hierarchy

```
Application (no lock)
    │
    ├─→ DashMap operations (per-entry lock)
    │   ├─ ConsensusEngine.timevote.tx_state (per-txid lock)
    │   ├─ ConsensusEngine.timevote.active_rounds (per-txid lock)
    │   ├─ TransactionPool.pending (per-txid lock)
    │   ├─ TransactionPool.finalized (per-txid lock)
    │   ├─ UTXOStateManager.utxo_states (per-outpoint lock)
    │
    ├─→ ArcSwap operations (lock-free, atomic)
    │   ├─ ConsensusEngine.masternodes (lock-free swap)
    │
    ├─→ OnceLock operations (lock-free, set-once)
    │   ├─ ConsensusEngine.identity (set at startup)
    │
    ├─→ AtomicUsize operations (lock-free)
    │   ├─ TransactionPool.pending_count
    │   ├─ TransactionPool.pending_bytes
    │
    └─→ RwLock operations (reader-friendly)
        ├─ Voting state (many readers during consensus)
        ├─ QueryRound votes (collector updates)
```

### Async Runtime Isolation

**CPU-Intensive Work (moved off runtime):**
- ✅ Ed25519 signature verification (`spawn_blocking`)
- ✅ Sled I/O operations (`spawn_blocking`)
- ✅ Serialization/deserialization (in blocking context)

**Async Work (on runtime):**
- ✅ Network I/O and message relay
- ✅ Task coordination
- ✅ Timeout handling (vote collection windows)
- ✅ State updates (via lock-free structures)
- ✅ TimeVote round scheduling
- ✅ TimeLock slot timing

---

## Error Handling

**Unified Error Type:**
```rust
#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    
    #[error("Consensus error: {0}")]
    Consensus(String),
    
    #[error("Network error: {0}")]
    Network(String),
}
```

**Error Propagation:**
- All async functions return `Result<T, AppError>`
- Main function catches and logs errors
- Graceful shutdown triggered on fatal errors

---

## Performance Characteristics

| Operation | Time Complexity | Space Complexity | Notes |
|-----------|-----------------|------------------|-------|
| Get UTXO | O(1) | O(1) | Lock-free DashMap |
| Add transaction | O(1) | O(n) | Atomic counter update |
| Check consensus | O(m) | O(m) | m = votes in round |
| List pending txs | O(n) | O(n) | n = pending count |
| Handle vote | O(1) | O(1) | Per-height lock |
| Route vote | O(1) | O(1) | Block hash index |
| Get connection count | O(1) | O(1) | Atomic counter |

---

## Scalability

**Horizontal Scaling:**
- Per-height BFT rounds enable parallel consensus
- DashMap enables many concurrent voters
- Lock-free primitives prevent contention

**Vertical Scaling:**
- Atomic counters for O(1) metrics
- Batch operations for database efficiency
- spawn_blocking prevents async runtime saturation

**Resource Limits:**
- Max 10,000 pending transactions
- Max 300MB pending transaction memory
- Max 50 peer connections
- Vote cleanup on finalization

---

## Deployment Architecture

```
┌────────────────────────────────────────┐
│  Load Balancer / DNS                   │
└────────────────────┬───────────────────┘
                     │
        ┌────────────┼────────────┐
        │            │            │
        ▼            ▼            ▼
    ┌───────┐   ┌───────┐   ┌───────┐
    │Node 1 │   │Node 2 │   │Node 3 │
    │Master │   │Master │   │Master │
    └───┬───┘   └───┬───┘   └───┬───┘
        │            │            │
        └────────────┼────────────┘
                     │
            P2P Mesh Network
                (Gossip)
                     │
        ┌────────────┼────────────┐
        │            │            │
        ▼            ▼            ▼
    ┌───────┐   ┌───────┐   ┌───────┐
    │Node 4 │   │Node 5 │   │Node 6 │
    │ Full  │   │ Full  │   │ Full  │
    └───────┘   └───────┘   └───────┘
```

**Minimum:** 3 masternodes for quorum (67% stake-weighted majority)  
**Recommended:** 5+ masternodes for redundancy  
**Full nodes:** Can be unlimited

---

## Security Considerations

| Aspect | Implementation |
|--------|----------------|
| **Message Authentication** | Ed25519 signatures |
| **Double-Spend Prevention** | UTXO locking mechanism |
| **Byzantine Tolerance** | Stake-weighted consensus (67% quorum, BFT-safe) |
| **Sybil Protection** | Masternode registry |
| **Network Privacy** | Optional encryption layer |
| **DOS Protection** | Rate limiting per peer |

---

**Last Updated:** 2025-12-24  
**Architecture Version:** 2.1 (Code cleanup)

---

## Complete Transaction & Consensus Flow

*Based on actual code analysis (codebase version 2026-02-16). Sections that duplicate content already covered above are omitted.*

---

### Node Startup Sequence (`main.rs`)

#### Initialization Order

1. **Parse CLI args** — config path, listen addr, masternode flag, verbose, demo, generate-config
2. **Print hostname banner** — node identity display
3. **Determine network type** — Mainnet or Testnet from config
4. **Setup logging** — tracing-subscriber with systemd detection, hostname prefix
5. **Open sled databases**:
   - `{data_dir}/db/peers` — Peer manager storage
   - `{data_dir}/db/registry` — Masternode registry
   - `{data_dir}/db/blocks` — Block storage (`flush_every_ms(None)`, `Mode::LowSpace`)
   - `{data_dir}/db/txindex` — Transaction index
6. **Initialize UTXO storage** — InMemoryUtxoStorage (or SledUtxoStorage)
7. **Initialize UTXOStateManager** — Loads UTXO states from storage
8. **Initialize PeerManager** — Peer discovery and tracking
9. **Initialize MasternodeRegistry** — With peer manager reference
10. **Initialize ConsensusEngine** — With masternode registry and UTXO manager
11. **Initialize AI System** — All 7 AI modules in AISystem struct (+ 3 wired separately) with shared sled Db
12. **Enable AI Transaction Validation** — On consensus engine
13. **Initialize Blockchain** — With block storage, consensus, registry, UTXO, network type
14. **Set AI System on Blockchain** — For intelligent decision recording
15. **Configure block compression** — Currently forced OFF
16. **Initialize Transaction Index** — For O(1) TX lookups
17. **Verify chain height integrity** — Fix inconsistencies from crashes
18. **Validate genesis block** — Create if needed, verify if exists
19. **Initialize TimeSync** — NTP-based time synchronization
20. **Start PeerConnectionRegistry** — Connection tracking
21. **Start ConnectionManager** — Manages connection lifecycle
22. **Start PeerStateManager** — Peer state machine
23. **Wire AI System on NetworkServer** — For attack enforcement
24. **Spawn block production task** — Event-driven + 10-minute interval TimeLock consensus
25. **Spawn status report task** — 60-second interval with AI reporting
26. **Spawn cleanup task** — 10-minute interval for memory management
27. **Start RPC server** — HTTP JSON-RPC interface
28. **Start NetworkServer** — Inbound peer connections (attack enforcement every 30 s)
29. **Start NetworkClient** — Outbound peer connections with adaptive reconnection
30. **Wait for shutdown** — Ctrl+C signal
31. **Flush sled to disk** — Critical: prevents block corruption

#### Genesis Block Handling

- If no genesis exists: create one with `Blockchain::create_genesis_block()`
- Genesis timestamp is network-type specific (Mainnet vs Testnet)
- Genesis block has height 0, `previous_hash = [0; 32]`
- Genesis is validated on startup: hash check, height-0 verification

---

### Block Production Flow

#### TimeLock Leader Selection

- **Block interval**: 600 seconds (10 minutes)
- **Leader selection**: VRF (ECVRF) using input:
  `SHA256("TIMECOIN_VRF_V2" || height_le_bytes || previous_hash)`
- Each masternode evaluates its own VRF proof; the single highest output wins
- Fallback leader rotation uses `TimeLock-leader-selection-v2` input on timeout

#### Block Production Loop (Event-Driven + Interval)

The main block production loop uses `tokio::select!` with four branches:
1. **Shutdown signal** — graceful exit
2. **Production trigger** — immediate wake when status check detects chain is behind
3. **`block_added_signal.notified()`** — event-driven wake when any block is added (sync, consensus, or own production), reducing latency to near-instant
4. **`interval.tick()`** — periodic 1-second fallback polling

#### Two-Phase Commit (2PC) for Block Finality

**Phase 1 — Propose:**
1. Leader assembles block from transaction pool
2. Broadcasts `TimeLockBlockProposal { block }` to all peers
3. Validators verify: valid transactions, correct previous hash, valid merkle root

**Phase 2a — Prepare Votes:**
1. Validators send `TimeVotePrepare { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PREPARE"`
3. Votes accumulate by validator **stake weight** (not raw count)
4. Threshold: >50% of participating validator weight

**Phase 2b — Precommit Votes:**
1. After prepare threshold met, validators send `TimeVotePrecommit { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PRECOMMIT"`
3. Threshold: >50% of participating validator weight
4. Block is finalized after precommit threshold

**TimeProof Finality (separate from 2PC):**
- Transactions achieve instant finality via TimeProof with **67% weighted stake** threshold (liveness fallback to 51% after 30 s)
- Weight is tier-based **sampling weight**: Free=1, Bronze=10, Silver=100, Gold=1000
- This is distinct from tier pool allocation and governance voting power

**Liveness Fallback:**
- Stall detection timeout: 30 seconds without consensus progress
- Broadcasts `LivenessAlert`, enters `FallbackResolution` state
- Up to 5 fallback rounds with 10-second round timeout each
- Deterministic hash-based leader selection per fallback round:
  `leader = MN with min SHA256(txid || slot_index || round || mn_address)`
- If validator count < 3 (early network / single node), block is added directly without votes

#### Block Structure

```
Block {
    header: BlockHeader {
        version: u32,
        height: u64,
        previous_hash: Hash256,
        merkle_root: Hash256,
        timestamp: i64,
        block_reward: u64,
        leader: String,
        attestation_root: Hash256,
        masternode_tiers: MasternodeTierCounts,
        vrf_proof: Vec<u8>,
        vrf_output: Hash256,
        vrf_score: u64,
        active_masternodes_bitmap: Vec<u8>,
        liveness_recovery: Option<bool>,
    },
    transactions: Vec<Transaction>,
    masternode_rewards: Vec<(String, u64)>,
    time_attestations: Vec<TimeAttestation>,
    // Direct voters (TimeVote prepare/precommit) MERGED with gossip-active masternodes.
    // Ensures pyramid-topology nodes not directly connected to producer appear in the
    // bitmap and remain eligible for rewards/VRF in the next block.
    consensus_participants_bitmap: Vec<u8>,
    liveness_recovery: Option<bool>,
}
```

#### Block Storage Key Formats

- Key format: `block_{height}` (current)
- Legacy `block:{height}` and BlockV1 schema migration supported in code but unused
- Chain height: `chain_height` key, bincode-serialized `u64`
- Chain tip: `tip_height` key, little-endian `u64` bytes
- Each write calls `db.flush()` with immediate readback verification
- Two-tier block cache: hot (deserialized) + warm (serialized) for 10–50× faster reads

---

### Transaction Flow

#### Transaction Structure

```
Transaction {
    inputs:         Vec<TxInput>,
    outputs:        Vec<TxOutput>,
    lock_time:      u64,
    tx_type:        TransactionType,
    encrypted_memo: Option<Vec<u8>>,   // ECDH + AES-256-GCM encrypted memo
}

TransactionType: Standard, CoinbaseReward, MasternodeReward,
                 MasternodeLock, MasternodeUnlock, GovernanceVote,
                 TimeProof, SmartContract
```

**Encrypted Memo:** Transactions may carry an optional encrypted memo (max 256 chars plaintext). The memo is encrypted using ECDH key exchange (Ed25519 → X25519 conversion) + AES-256-GCM so that only the sender and recipient can decrypt it. The wire format stores both sender and recipient Ed25519 public keys, a random nonce, and the ciphertext. Wallet-generated consolidation and merge transactions automatically attach descriptive memos (e.g., "UTXO Consolidation"). See §4.5 in the protocol spec for details.

#### Transaction Processing Steps

1. **Receive**: `TransactionBroadcast` message from peer
2. **Dedup**: Check SeenTransactions filter (bloom-filter-like)
3. **AI Attack Detection**: Record transaction for double-spend tracking
4. **Consensus Processing**: `ConsensusEngine::process_transaction()`
   - Validate against UTXO set
   - AI transaction validation (spam/dust detection)
   - Add to transaction pool
5. **Gossip**: Broadcast to other connected peers
6. **TimeVote Finality**: Instant finality via TimeVote consensus

#### Per-Transaction State Machine (`TransactionStatus`)

```
Seen → Voting → Finalized → Archived
         │          ↑
         │     (accumulated_weight ≥ Q_finality, TimeProof complete)
         │
         ├→ FallbackResolution → Finalized / Rejected
         │   (stall > 30s, deterministic leader resolves)
         │
         └→ Rejected
             (conflict lost or invalid)
```

- **Seen**: Transaction received, pending validation
- **Voting**: Actively collecting signed FinalityVotes; tracks `accumulated_weight` and `confidence`
- **FallbackResolution**: Stall detected; deterministic fallback round in progress (tracks round number and alert count)
- **Finalized**: `accumulated_weight ≥ 67%` of AVS weight; TimeProof assembled
- **Rejected**: Lost conflict resolution or deemed invalid
- **Archived**: Included in TimeLock checkpoint block

#### Transaction Pool Details

- **Three-map structure**:
  - `pending` — Transactions in consensus (Seen + Voting states): `DashMap<Hash256, PoolEntry>`
  - `finalized` — Transactions ready for block inclusion: `DashMap<Hash256, PoolEntry>`
  - `rejected` — Previously rejected transactions with reason and timestamp: `DashMap<Hash256, (String, Instant)>`
- Max pool size: 100 MB (configurable)
- Pressure levels: Normal (0–60%), Warning (60–80%), Critical (80–90%), Emergency (90%+)
- Priority scoring: fee rate, age, TX type
- Eviction: lowest priority first when pool is full
- Rejected TX cleanup: after 1 hour

#### UTXO State Machine

Five states (not the typical 2):

- **Unspent**: Available for spending
- **Locked**: Masternode collateral — cannot be spent; created by `MasternodeLock` transaction
- **SpentPending**: Input locked during TimeVote consensus; tracks `txid`, vote counts, `spent_at`
- **SpentFinalized**: Transaction finalized with votes (51% or 67% threshold reached)
- **Archived**: Included in block; final on-chain state

Collateral locking includes a 10-minute timeout cleanup for orphaned locks.

**Spent tombstone invariant (v1.5.0+):** Once an outpoint transitions to any spent state it is permanently recorded in a `spent_tombstones: DashSet<OutPoint>` (also persisted to the sled `spent_utxos` tree). `add_utxo` rejects the outpoint with `AlreadySpent` regardless of what `utxo_states` or sled storage contain. The tombstone is only lifted by `restore_utxo` during an explicit rollback (fork resolution), and cleared entirely by `clear_all` at the start of a reindex.

---

### Network Sync and Fork Resolution Flow

#### Sync Flow

1. Node starts → checks current height vs expected height
2. If behind: calls `sync_from_peers(None)`
3. `sync_from_peers()`:
   - Gets connected peers from peer registry
   - Requests blocks from `current_height + 1` up to peer's height
   - Processes blocks sequentially
   - Stops at first missing block (no gap tolerance)
4. **Sync Coordinator** prevents storms:
   - Rate-limits sync requests
   - Tracks active sync operations
   - Prevents duplicate sync to the same height range

#### Fork Resolution Flow

**Prerequisite — genesis confirmation:** Fork resolution only engages with `genesis_confirmed` peers. A peer becomes genesis-confirmed when `handle_genesis_hash_response` or `verify_genesis_compatibility` verifies its genesis hash matches ours. Peers that never respond to genesis requests are marked temporarily incompatible (5-minute cooldown) and are excluded from fork decisions to prevent old-code nodes from triggering endless reorg attempts.

Chain comparison in `compare_chain_with_peers()` (`blockchain.rs`):
1. **Height-first** (primary): longest chain wins
2. **Stake tiebreaker** (same height): higher cumulative `sampling_weight()` wins — Free=1, Bronze=10, Silver=100, Gold=1000
3. **Peer count** (same height + weight): more supporting peers wins
4. **Deterministic hash** (final): lexicographically lower block hash wins

`handle_fork()` decision flow (`blockchain.rs`):
1. Find common ancestor via binary search
2. Security checks: reject genesis reorgs, reject depth > 500 blocks, reject future timestamps
3. Call `fork_resolver.resolve_fork()`: longer chain always wins; same height → stake tiebreaker, then hash
4. If accepted, perform reorg: roll back to ancestor, replay peer chain

Fork alert protocol (`message_handler.rs`):
- When we're ahead: send `ForkAlert` to lagging peers (rate-limited to once per 60 s per peer)
- When peer is ahead and in consensus: request blocks to sync
- On receiving `ForkAlert`: request blocks from consensus chain if behind or hash differs
- Validations: timestamp, merkle root, signatures, chain continuity
- Finalized transaction protection: reject forks that would reverse finalized transactions

---

## TimeProof Conflict Detection

TimeProof conflict detection is a **security monitoring feature** that detects and logs anomalies indicating implementation bugs or Byzantine validator behavior. It does NOT prevent double-spends — that is handled by UTXO locking.

### Key Insight from Protocol Analysis

**By pigeonhole principle**, two transactions spending the same UTXO cannot both reach 67% finality:
- TX-A needs 67% weight = 6700 units (of 10,000 total)
- TX-B needs 67% weight = 6700 units
- Total: 13,400 > 10,000 — mathematically impossible

Therefore, multiple finalized TimeProofs for the same transaction indicates:
1. **UTXO state machine bug** — should have rejected one transaction at the validation layer
2. **Byzantine validator equivocation** — voting for conflicting transactions
3. **Stale proof** — from a network partition that lost consensus

### Data Structures (`src/types.rs`)

```rust
pub struct TimeProofConflictInfo {
    pub txid:                Hash256,
    pub slot_index:          u64,
    pub proof_count:         usize,   // Number of competing proofs
    pub proof_weights:       Vec<u64>,// Weight of each proof
    pub max_weight:          u64,     // Highest weight (winner)
    pub winning_proof_index: usize,   // Index of winning proof
    pub detected_at:         u64,     // Timestamp when detected
    pub resolved:            bool,    // Has conflict been resolved?
}
```

### Core Methods (`src/consensus.rs`)

#### `detect_competing_timeproof(proof: TimeProof, weight: u64) -> Result<usize, String>`
- Called when a new TimeProof is received
- If competing proofs exist → logs anomaly
- Returns index of winning proof (highest weight)
- Updates metrics: `timeproof_conflicts_detected`

#### `resolve_timeproof_fork(txid: Hash256) -> Result<Option<TimeProof>, String>`
- Selects canonical proof (highest accumulated weight)
- Marks conflict as resolved
- Used for partition healing reconciliation

#### `get_competing_timeproofs(txid: Hash256) -> Vec<TimeProof>`
- Retrieves all proofs for a transaction
- Used for security analysis

#### `get_conflict_info(txid: Hash256, slot_index: u64) -> Option<TimeProofConflictInfo>`
- Gets detailed conflict information for AI anomaly detector and monitoring dashboards

#### `conflicts_detected_count() -> usize`
- Metrics counter for security monitoring

### Test Coverage

8 comprehensive tests covering all scenarios:

| Category | Tests |
|----------|-------|
| Normal operation | `test_single_timeproof_no_conflict`, `test_competing_proofs_should_never_happen_normally` |
| Anomaly detection | `test_competing_timeproofs_detected_as_anomaly`, `test_stale_proof_detection_from_partition` |
| Fork resolution | `test_fork_resolution_selects_canonical`, `test_clear_competing_timeproofs_after_investigation` |
| Monitoring & metrics | `test_conflict_metrics_for_monitoring`, `test_conflict_info_for_security_alerts` |

All 8 tests pass.

### Usage

```rust
// When a TimeProof arrives from the network
let winning_idx = consensus.detect_competing_timeproof(proof, weight)?;
if winning_idx != 0 {
    tracing::warn!("Proof replaced - potential partition/Byzantine behavior");
}

// In security monitoring loop
let total_conflicts = consensus.conflicts_detected_count();
if let Some(conflict) = consensus.get_conflict_info(txid, slot_index) {
    alert_security_dashboard(conflict);
}

// After partition healing
let canonical = consensus.resolve_timeproof_fork(txid)?;
```

### Integration Points

| Layer | Role |
|-------|------|
| **Blockchain layer** | When adding finalized TX to block, check for conflicts; log alert and select canonical proof if found |
| **UTXO Manager** | Verify conflicting transactions were rejected at validation layer; conflicting TimeProofs indicate a state machine bug |
| **AI Anomaly Detector** | Feed conflict info to anomaly model; train on weight ratios, vote patterns, validator behavior |
| **Network layer** | Optional `ConflictNotification` message for partition healing coordination; broadcast winning TimeProof |

### Security Properties

| Property | Description |
|----------|-------------|
| **Byzantine detection** | Multiple signatures on conflicting proofs are caught |
| **Deterministic resolution** | Weight-based selection ensures unambiguous canonical outcome |
| **Partition-safe** | Minority partition's proof is marked as stale |
| **Non-blocking** | Node continues operating while investigating |
| **Audit trail** | All conflicts logged with timestamps and weights |

### Performance

- **Detection**: O(1) — constant-time conflict recording
- **Resolution**: O(N) where N = number of competing proofs (typically 2)
- **Memory**: O(N × M) where N = # transactions with conflicts, M = # proofs per transaction
- **Normal case**: Zero overhead (single proof per transaction)

### What This Does NOT Do

- ❌ Prevent double-spends (UTXO locking does that)
- ❌ Handle consensus forks (TimeGuard fallback does that)
- ❌ Banlist validators (AI anomaly detector does that)
- ❌ Require network coordination (works unilaterally)

### Future Enhancements

1. **Network-wide conflict propagation** — broadcast `ConflictNotification` for coordination
2. **Validator reputation** — feed into Byzantine node detection system
3. **Automated slashing** — slash validators caught equivocating (if slashing is implemented)
4. **Dashboard integration** — real-time security monitoring UI

