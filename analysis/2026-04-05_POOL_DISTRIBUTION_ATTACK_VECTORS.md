# Pool Distribution Attack Vectors & Fixes — 2026-04-05

## Context

Chain stall incident observed starting around block 671 on mainnet.
Multiple nodes were offline, restarted, and the network could not advance
past block 676. Every block proposal from every producer was rejected by
every validator. This document catalogues the attack vectors and bugs
diagnosed and fixed during the incident.

---

## Attack Vector 1: Non-Deterministic Tier Classification (Wallet Overlap)

### Severity: High — Causes chain stall

### Description

When a masternode operator registers the **same wallet address** at multiple
tiers (e.g., a Silver node at IP_A and a Free node at IP_B), the function
`tier_for_wallet` previously iterated a `HashMap` and returned the tier of
whichever entry happened to be first in iteration order. HashMap iteration
order is non-deterministic across runs and between nodes.

**Effect:** Different nodes classify the same wallet as Silver on one node
and Free on another. This causes:
- Block producer pays the wallet from the Silver pool
- Some validators expect the Silver pool to be paid (agreeing)
- Other validators expect a Free pool payment (disagreeing)
- Result: inconsistent block validation → proposals rejected

### Example (block 671)

- LW-Michigan: "distributed 160M satoshis for Silver, expected 1,800M"
- LW-Michigan2: "distributed 1,960M satoshis for Silver, expected 1,800M"
- Root cause: same wallet registered at Silver (IP_A) and Free (IP_B).
  One node classified it as Silver, another as Free, changing both the
  Silver and Free pool amounts.

### Fix

`tier_for_wallet` now returns the **highest tier** when a wallet appears in
multiple registrations. Deterministic across all nodes regardless of HashMap
iteration order.

```rust
// masternode_registry.rs — tier_for_wallet
.max_by_key(|tier| *tier as u64)
```

Discriminant values ensure correct ordering: Gold=100000 > Silver=10000 >
Bronze=1000 > Free=0.

---

## Attack Vector 2: Free Pool Double-Payment (Wallet Overlap in Block Production)

### Severity: High — Causes consensus mismatch / incorrect reward distribution

### Description

When an operator runs both a paid-tier node (Silver/Bronze/Gold) and a
Free-tier node with the **same wallet address**, the block producer was
including the Free node in the Free tier pool distribution. This means:

1. The wallet gets paid from the paid-tier pool (Silver/Bronze/Gold)
2. The wallet also gets paid from the Free pool
3. Validators that correctly identify the wallet as a paid-tier wallet
   reject the Free pool output as invalid

This is also a direct **economic exploit**: an operator could extract extra
rewards by registering both a paid-tier and Free-tier node with the same
wallet.

### Fix

Block production now builds a `paid_tier_wallet_set` (all wallets receiving
paid-tier payments in this block) **before** processing the Free tier. Free
nodes whose destination wallet is in this set are excluded from Free pool
distribution.

```rust
// blockchain.rs — before the tier loop
let paid_tier_wallet_set: HashSet<String> = eligible_pool
    .iter()
    .filter(|mn| mn.masternode.tier != MasternodeTier::Free)
    .map(|mn| reward_address_or_wallet(mn))
    .collect();

// Inside tier loop — Free tier only
if matches!(tier, MasternodeTier::Free) {
    tier_nodes.retain(|mn| !paid_tier_wallet_set.contains(dest));
}
```

---

## Attack Vector 3: Gossip State vs. Bitmap Divergence (Post-Restart Stall)

### Severity: Critical — Can permanently stall the chain

### Description

This is the most serious vector and the root cause of the block 676 stall.

**Background:** The `active_masternodes_bitmap` committed in each block header
records which masternodes participated as consensus voters in the *previous*
block. The `is_active` gossip flag is set when ≥3 peers report a masternode
recently (within 5 minutes), independent of actual consensus participation.

**Attack/Failure scenario:**

1. A set of masternodes go offline (maintenance, crash, network partition).
2. The nodes come back online. Gossip quickly propagates their presence,
   setting `is_active = true` on all peers within ~5 minutes.
3. However, these nodes have **not yet participated in consensus** — they
   have not cast any precommit votes, so they do not appear in the bitmap
   of any recent block.
4. The `validate_pool_distribution` function previously used `list_by_tier`
   (gossip-based) when `paid == 0` to count "expected" tier nodes.
5. Validators with gossip-restored nodes see: "8 Bronze nodes are active →
   the Bronze pool MUST be distributed." Producers see: "0 Bronze nodes are
   in the bitmap → pool rolls up to producer."
6. **Every block proposal from every producer fails validation on every
   validator.** The chain halts.

**Why this is an attack vector (not just a bug):**

A malicious actor who controls a significant stake could:
- Deliberately cycle masternodes offline/online to trigger gossip divergence
- Time the re-activation to coincide with their own block production slot
- Benefit from pools rolling up to them while honest validators reject all
  competing proposals

### Fix

In `validate_pool_distribution`, when `paid == 0`, use the block's committed
`active_masternodes_bitmap` instead of `list_by_tier`:

```rust
// blockchain.rs — validate_pool_distribution
if paid == 0 {
    let non_producer_tier_nodes = if !block.header.active_masternodes_bitmap.is_empty() {
        let bitmap_nodes = self.masternode_registry
            .get_active_from_bitmap(&block.header.active_masternodes_bitmap)
            .await;
        bitmap_nodes.iter().filter(|info| {
            info.masternode.tier == *tier && wallet != producer_addr
        }).count()
    } else {
        // Old block without bitmap — fall back to gossip
        self.masternode_registry.list_by_tier(*tier).await
            .iter().filter(...).count()
    };
    if non_producer_tier_nodes > 0 {
        return Err("tier pool not distributed: N node(s) received 0");
    }
}
```

**Why the bitmap is the correct source of truth:**
- It is part of the block hash — tamper-evident
- It requires actual consensus participation to appear in it
- It is identical on all nodes that share the same chain state
- Gossip state is local, non-deterministic, and can diverge silently

**Why gossip was wrong here:**
- A node that just restarted appears `is_active` via gossip almost immediately
- But it hasn't voted in any block yet → absent from every recent bitmap
- The producer correctly skips it (not in bitmap) → pool rolls up
- Old validators incorrectly reject it (gossip says it's active, must be paid)

---

## Systemic Vulnerability: Gossip as Consensus Input

All three vectors above stem from the same root weakness: **using gossip
state (`is_active`) as an input to consensus validation**.

Gossip is:
- Eventually consistent (not immediately consistent)
- Local to each node
- Unverifiable at the time of block validation
- Manipulable by timing of connect/disconnect cycles

The bitmap is:
- Derived from verifiable BFT precommit signatures
- Identical across all nodes with the same chain state
- Tamper-evident (included in block hash)
- Requires real consensus participation

**Recommendation:** Audit all remaining places in `validate_pool_distribution`
and reward eligibility logic for any remaining reliance on gossip `is_active`.
Consider a full migration to bitmap-gated reward eligibility for paid tiers.

---

---

## Attack Vector 4: Collateral Anchor Squatting

### Severity: High — Prevents legitimate node from registering; inflates wallet balance display

### Description

The masternode gossip system uses a **first-claim anchor** stored in sled under
`collateral_anchor:{txid}:{vout}`. The first IP to broadcast a `MasternodeAnnouncement`
referencing a given collateral outpoint "owns" that anchor permanently. Any later
announcement from a different IP is silently rejected.

**Attack scenario:**

1. Operator sends a TIME send-to-self transaction (TXID: `926b2fb0...`) to create a
   Silver collateral UTXO from their GUI wallet.
2. An attacker (`47.82.240.140`) monitors the mempool, sees the TXID, and gossips a
   `MasternodeAnnouncement` claiming `926b2fb0:0` before the legitimate node (`188.166.243.108`)
   can announce itself.
3. The attacker's anchor is written to sled first.
4. When the legitimate node (`188.166.243.108`) announces, it is rejected with
   `CollateralAlreadyLocked` — even though it controls the UTXO.
5. The attacker receives Silver rewards it is not entitled to. The legitimate operator's
   4th Silver node never appears on other nodes' registries.

**Secondary effect — wallet balance inflation:**

Each time a node restarts, it restores collateral lock entries from sled. Remote nodes'
locks (from other operators' gossip) were also persisted, causing the sled lock database
to grow unboundedly. The wallet balance display counted all `LockedCollateral` entries
against "locked balance", so a node with 14 attacker-squatted locks showed 52,000 TIME
locked instead of the correct 41,000 TIME.

### Fix (commit `d7c2de1` — 2026-04-05)

**Collateral lock persistence fix:**

Split `lock_collateral` into `lock_local_collateral` (persists to sled) and
`lock_collateral` (in-memory only). Remote nodes' locks are kept in-memory for
UTXO spend prevention during the session but are never written to sled. This stops
the unbounded growth of the lock database on restart.

**Wallet balance fix:**

`get_balance` and `getwalletinfo` now only count a `LockedCollateral` entry as
"locked balance" if `lock.masternode_address == local_node_ip`. Foreign locks
fall through to the normal spendable/unspendable check.

**Registry address check (partial mitigation):**

`register_internal` gained a check: if a squatter's anchor exists, look up the
UTXO's on-chain address and compare it to the incoming announcement's
`wallet_address`. If they match, evict the squatter.

**Known limitation of `d7c2de1`:** The `wallet_address` field comes from the
announcement message itself — an attacker can set it to any value, including the
victim's address, making the check exploitable. Additionally, `handle_masternode_announcement`
in the message handler already returns early at the collateral-lock conflict check
before `register_internal` is ever reached for gossip messages, making the
registry-level check unreachable in practice.

---

### Fix (commit `6e6d14e` — 2026-04-06) — **V4 Collateral Proof**

**Root cause of the gap in `d7c2de1`:** The `wallet_address` in a gossip
announcement is self-reported and unverifiable. The only keys held by the
masternode daemon that are not self-reported are `masternodeprivkey` (in
`time.conf`) and the collateral UTXO outpoint (in `masternode.conf`). In the
operator's configuration, `reward_address` is set to the **same address as the
collateral UTXO's output address** — i.e. the GUI wallet address the coins were
sent to.

**Solution — self-signed collateral proof in V4 announcements:**

The masternode daemon signs a binding message at announcement time:

```
"TIME_COLLATERAL_CLAIM:<txid_hex>:<vout>"
```

using its `masternodeprivkey`. This signature goes in the `collateral_proof`
field of `MasternodeAnnouncementV4`. No GUI wallet changes are required — the
daemon already has everything it needs.

Conflict resolution in `handle_masternode_announcement` is updated with a two-part
ownership test:

1. **Valid proof**: `public_key.verify("TIME_COLLATERAL_CLAIM:<txid>:<vout>", sig)` —
   proves the announcing masternode key is bound to this specific UTXO outpoint.
2. **On-chain address match**: `reward_address == utxo.address` — the announced
   reward address matches the address recorded on-chain when the collateral was
   created. This is immutable chain data, not self-reported gossip.

If both conditions pass and the existing lock is held by a different IP, the
squatter is **evicted**: its collateral lock is released, its registry entry
removed, and the legitimate owner is registered.

**Why an attacker cannot bypass this:**

- To pass condition (1), the attacker must sign with the *victim's* masternode
  private key — which they do not have.
- To pass condition (2) with their own key, the attacker must set
  `reward_address = victim's wallet address`, meaning all earned rewards are
  sent to the victim's wallet. The attack has no financial upside.
- To pass both with their own key *and* their own reward address, condition (2)
  fails because their `reward_address ≠ utxo.address`.

**Residual edge case (V4 vs V4 race):** If an attacker also sends V4 (signing
their own key over the UTXO and setting `reward_address = victim's address`), a
race condition exists. However: the daemon re-announces every 60 seconds, so the
legitimate operator reclaims their registration within one cycle. The attacker
must continuously maintain the squat while donating all rewards to the victim —
making the attack economically irrational.

```rust
// message_handler.rs — handle_masternode_announcement (simplified)
let can_evict = if !collateral_proof.is_empty() && reward_address == utxo.address {
    let proof_msg = format!("TIME_COLLATERAL_CLAIM:{}:{}", txid_hex, outpoint.vout);
    Signature::from_slice(&collateral_proof)
        .map(|sig| public_key.verify(proof_msg.as_bytes(), &sig).is_ok())
        .unwrap_or(false)
} else {
    false
};

if can_evict {
    utxo_manager.unlock_collateral(&outpoint)?;
    registry.unregister(&squatter_ip).await?;
    // fall through to lock and register the legitimate owner
} else {
    return Ok(None); // first-claim wins (V3 behaviour)
}
```

The relay path was also updated: when a legitimate owner's V4 announcement is
forwarded to other peers, it is relayed as V4 (with proof intact), so all nodes
can apply the eviction independently without needing to witness the initial
conflict.

### Fix (commit `45bb9ba` — 2026-04-09) — **Disconnect on hijack attempt**

**Gap in prior fixes:** `CollateralAlreadyLocked` in `message_handler.rs` called
`blacklist.record_severe_violation()` but discarded the `bool` return value, so the
attacker's TCP connection was never terminated. The ban was written to the blacklist
(blocking future connections) but the *current* connection stayed alive and the
attacker could keep flooding gossip indefinitely from it.

**Fix:** Both handlers (paid-tier and free-tier announcement paths) now propagate the
`should_disconnect` return value from `record_severe_violation`. When `true` (always
on first offense), the handler returns `Err("DISCONNECT: ...")`, which causes the
message loop to break and the connection to close.

```rust
// message_handler.rs — paid-tier CollateralAlreadyLocked handler
RegistryError::CollateralAlreadyLocked => {
    warn!("❌ [Inbound] Free-tier collateral hijack attempt from {} — recording violation", self.peer_ip);
    let should_disconnect = blacklist.record_severe_violation(&self.peer_ip, "collateral hijack attempt");
    if should_disconnect {
        return Err(format!("DISCONNECT: collateral hijack from {}", self.peer_ip));
    }
    Ok(None)
}
```

**Effect:** Squatter is disconnected on the first gossip flood message. Previously it
took a watchdog restart (or manual `time-cli ban`) to stop the flood.

---

## Attack Vector 5: Chain Split via Historical Pool Mismatch (Block 679)

### Severity: Medium — Splits minority nodes off the main chain

### Description

After the block 676 stall was resolved, the majority chain advanced to blocks
679–681 with pool distributions computed under the new bitmap-based validation.
Nodes that had been stuck at 678 (minority) tried to sync these blocks but
rejected them — not because the blocks were invalid, but because the nodes'
local gossip state at the time differed from what the block producers had seen
at blocks 679–681.

This is a consequence of the transition period: old nodes had gossip-divergent
state at those heights; the new bitmap validation code was not yet deployed on
all nodes.

### Fix (commit `9b388ed` — 2026-04-05)

Added `POOL_VALIDATION_MIN_HEIGHT = 682` in `src/constants.rs`. In
`validate_pool_distribution`, blocks below this height return `Ok(())` immediately,
allowing minority nodes to fast-forward through the split period and rejoin the
majority chain.

```rust
// blockchain.rs — validate_pool_distribution
if block.header.height < POOL_VALIDATION_MIN_HEIGHT {
    return Ok(());
}
```

This is a one-time bypass for the transition blocks. All blocks from 682 onward
use the full bitmap-based validation.

---

## Incident Timeline

| Block | Event |
|-------|-------|
| ~671  | Silver pool mismatch: LW-Michigan sees 160M distributed, LW-Michigan2 sees 1,960M distributed (expected: 1,800M). Root cause: shared wallet across Silver and Free registrations + non-deterministic `tier_for_wallet`. |
| 672–675 | Chain advances slowly; errors accumulate |
| 676 | Chain stalls completely. "Bronze-tier pool not distributed: 8 registered node(s) received 0" on every proposal from every producer. Root cause: gossip-restored Bronze nodes appear active but absent from bitmap. |
| 679–681 | Majority chain advances under new bitmap validation. Minority nodes (still at 678) reject these blocks due to gossip-divergent local state from the transition period. Chain splits. |
| 682+ | `POOL_VALIDATION_MIN_HEIGHT` bypass deployed. Minority nodes accept the split-period blocks and rejoin majority chain. |
| ongoing | Attacker (`47.82.240.140`) squat-registered `926b2fb0:0`, blocking DO-Singapore (`188.166.243.108`) from appearing as 4th Silver node on all peers. 52,000 TIME shown as locked in wallet (should be 41,000). |

---

## Fixes Deployed

| Commit | Date | Changes |
|--------|------|---------|
| `8d2086a` | 2026-04-05 | `tier_for_wallet` deterministic (highest tier wins); Free pool excludes paid-tier wallet overlaps; `validate_pool_distribution` uses bitmap for paid==0 check |
| `9b388ed` | 2026-04-05 | `POOL_VALIDATION_MIN_HEIGHT = 682` — bypass pool validation for pre-split blocks |
| `730cf2e` | 2026-04-05 | `MasternodeAnnouncementV4` with `collateral_proof` field (groundwork) |
| `cb6da17` | 2026-04-05 | `lock_local_collateral` — only persist local node's own collateral lock; foreign locks in-memory only; wallet balance filters to own locks only |
| `d7c2de1` | 2026-04-05 | Collateral squatter eviction via UTXO output address ownership proof; `set_utxo_manager` injected into registry at startup |
| `5034aa9` | 2026-04-06 | Block Tier 2 gossip eviction of local node in both UTXOManager-locked and registry-only eviction paths |
| `89bd02d` | 2026-04-06 | Gate `mark_inactive_on_disconnect` on `handshake_done` — pre-handshake connection failures no longer deregister masternodes |
| `12e4fb1` | 2026-04-06 | Block Free-tier gossip migration when existing holder is a paid tier; genesis hash timeout returns compatible (not incompatible) to prevent startup isolation |
| `73275c7` | 2026-04-06 | Evict gossip squatter from local registry on startup when `CollateralAlreadyLocked` — unregister squatter and re-register local node |
| `99e3718` | 2026-04-06 | Run full post-registration setup (consensus identity, UTXO pubkey, collateral lock, on-chain source flag) when evicting startup squatter |
| `2d842f6` | 2026-04-06 | Block V4 gossip eviction of local node at both eviction sites; add 60-second per-outpoint V4 eviction cooldown to kill multi-claimant eviction storms |

---

## Attack Vector 6: Free-Tier Migration Attack on Paid-Tier Holder

### Severity: High — Silently steals paid-tier collateral registration via gossip

### Description

After a Tier 2 (address-match) eviction restores the legitimate owner of a
collateral outpoint, the `register_internal` migration path in `masternode_registry.rs`
checked only whether the **incoming** announcement was a paid tier. A Free-tier
announcement from a different IP with a matching address could bypass this check
because the code path for paid-tier holders was never reached.

**Attack scenario:**

1. Attacker squats a paid-tier (e.g., Silver) collateral via a Tier 2 gossip
   announcement.
2. Legitimate operator triggers a Tier 2 eviction — restores correct registration.
3. Attacker immediately gossips a **Free-tier** announcement for the same collateral.
4. `register_internal` sees: incoming tier = Free (no paid-tier block needed).
   Migration fires. Attacker re-squats the registration.
5. The eviction cycle repeats indefinitely without cost to the attacker.

**Why Tier 2 eviction alone was insufficient:**

The Tier 2 eviction does not write a `collateral_anchor` to sled. Without a
persisted anchor, the next announcement for that outpoint is treated as a
fresh registration — the migration guard only checked `incoming_tier`.

### Fix (commit `12e4fb1` — 2026-04-06)

`register_internal` now checks the **existing holder's tier** before allowing any
migration, not just the incoming tier. If the existing holder is a paid tier, all
gossip migration is blocked regardless of what tier the incoming claim presents.
The attacker must file an on-chain `MasternodeReg` transaction to displace a
paid-tier holder.

```rust
// masternode_registry.rs — register_internal (simplified)
if let Some(existing_addr) = self.find_holder_of_outpoint(&outpoint).await {
    let existing_tier = self.get_tier_for_ip(&existing_addr).await;
    if existing_tier.is_paid() {
        warn!("🛡️ Collateral {} is registered to {} — {} must file on-chain MasternodeReg",
              outpoint, existing_addr, masternode.address);
        return Err(RegistryError::CollateralAlreadyLocked);
    }
}
```

---

## Attack Vector 7: Genesis Hash Timeout → Network Isolation

### Severity: Medium — Node becomes fully isolated from the network on every restart

### Description

When a node restarts, it opens outbound connections and sends `GetGenesisHash` to
each peer to verify network compatibility. Older nodes (running software that
predates the `GetGenesisHash` message type) silently ignore the request, causing
a 10-second timeout on the connecting node.

The timeout branch previously called `mark_incompatible(peer)`, which prevents
the peer from being used for sync for `INCOMPATIBLE_RECHECK_SECS = 300` seconds.
Since all active peers run older software simultaneously, every peer is marked
incompatible at the same time on every restart, isolating the node completely for
up to 5 minutes.

**Cascade effects:**

- `get_compatible_peers()` returns empty → sync coordinator stalls
- Fork detection falls back to the isolated node's own chain (no peers to compare)
- During the 5-minute isolation window, the node cannot receive blocks, votes, or
  masternode announcements

**Why this is exploitable:**

Any attacker who can delay or drop `GetGenesisHash` responses (e.g., via a
man-in-the-middle or by deploying many old-version nodes) can continuously extend
the isolation window beyond 5 minutes by timing re-check requests.

### Fix (commit `12e4fb1` — 2026-04-06)

In `peer_connection_registry.rs` `verify_genesis_compatibility()`, the timeout,
`Err`, "no genesis", and "unexpected response" branches now return `true` (assume
compatible) instead of calling `mark_incompatible`. Only an explicit hash
**mismatch** (i.e., the peer replies with a different genesis hash) marks the
peer incompatible.

```rust
// peer_connection_registry.rs — verify_genesis_compatibility
match response {
    Ok(NetworkMessage::GenesisHash { hash }) => {
        if hash == our_genesis { true }
        else { self.mark_incompatible(addr); false } // explicit mismatch only
    }
    _ => true, // timeout / unknown msg type — assume compatible
}
```

---

## Attack Vector 8: Startup Squatter Leaves Local Node Unregistered (Partial Fix Gap)

### Severity: High — Node starts successfully but never appears in any peer's registry

### Description

When the daemon starts and calls `registry.register()` for the local masternode,
it may receive `Err(CollateralAlreadyLocked)` because a gossip squatter has already
claimed the collateral outpoint. The original `main.rs` handler for this error
called `set_local_masternode()` to record the local IP — but then returned early,
skipping the entire successful-registration block:

- `consensus_engine.set_identity()` — signing key for precommit votes
- `consensus_engine.set_wallet_signing_key()` — signing key for transactions
- `utxo_manager.register_pubkey()` — pubkey cache for fast signature checks
- **`utxo_manager.lock_local_collateral()`** — marks collateral UTXO as locked
- `utxo_manager.rebuild_collateral_locks()` — restores all peers' locks from sled
- `registry.set_registration_source(OnChain)` — prevents gossip from overwriting
  the local entry on disconnect

Because `lock_local_collateral()` was skipped, `getwalletinfo` reported 0 locked
balance and the dashboard showed "Not a Masternode" (the collateral lock is what
the dashboard uses to identify the local node's tier).

### Fix (commits `73275c7` + `99e3718` — 2026-04-06)

The `CollateralAlreadyLocked` handler was replaced with a full eviction-and-retry
sequence:

1. Find the squatter via `find_holder_of_outpoint(outpoint)`
2. Call `registry.unregister(squatter_ip)` — removes the entry AND clears the
   `collateral_anchor` from sled, allowing re-registration
3. Re-call `registry.register(local_masternode)` — succeeds now that the anchor
   is gone
4. Execute the full successful-registration block, including `lock_local_collateral()`

```rust
// main.rs — CollateralAlreadyLocked branch (simplified)
Err(RegistryError::CollateralAlreadyLocked) => {
    if let Some(squatter) = registry.find_holder_of_outpoint(&outpoint).await {
        registry.unregister(&squatter).await?;
        registry.register(local_mn.clone()).await?;
        // ... full post-registration setup including lock_local_collateral() ...
    }
}
```

---

## Attack Vector 9: Tier 2 Gossip Eviction of Local Node

### Severity: High — Removes local node from all peers' registries without proof

### Description

The three-tier eviction priority in `handle_masternode_announcement` includes a
"Tier 2 — address match" path: if the incoming announcement's `reward_address`
matches the collateral UTXO's on-chain address and the existing holder's
`reward_address` does not match, the existing holder is evicted.

The Tier 2 path did not check whether the existing holder was the **local node**.
An attacker who knows the UTXO address (which is public on-chain) can set
`reward_address = victim's UTXO address` in a gossip announcement and displace the
local node from the registry without any cryptographic proof — the signature (V4)
is only required to break an address-match stalemate, not to initiate a Tier 2
eviction.

**Effect:** Local node disappears from all connected peers' registries. It stops
receiving rewards. The dashboard shows the correct local state (the local registry
still has its own entry) but peers stop routing votes and announcements to it.

### Fix (commit `5034aa9` — 2026-04-06)

In `handle_masternode_announcement`, both the UTXOManager-locked and registry-only
Tier 2 eviction paths now check `is_local_node(existing_holder_ip)`. If the existing
holder is the local node, eviction is blocked regardless of whether the incoming
announcement has a matching reward address.

```rust
// message_handler.rs — Tier 2 eviction guard
if is_local_node(&existing_ip, &context.local_ip) {
    warn!("🛡️ Blocked Tier 2 eviction of local node by {}", peer_ip);
    return Ok(None);
}
```

---

## Attack Vector 10: Registry Conflict Log Flood (Sybil Denial-of-Service)

**Observed:** April 7, 2026 — LW-Michigan2  
**Severity:** Medium — log exhaustion + CPU/IO waste; no direct financial harm

### Description

The "Registry conflict" code path in `message_handler.rs` (the `can_evict == false`
branch) fired on every `MasternodeAnnouncement` from a node that does not hold a
valid V4 proof for the claimed outpoint. The path:

1. Logged a `WARN` line every single time
2. Recorded **zero violations** — no cost to the attacker whatsoever
3. Returned `Ok(None)` silently

Nodes in the `154.217.246.0/24` Sybil subnet exploited this by broadcasting
announcements for outpoints they did not own. With ~15 attacking IPs sending
announcements simultaneously, the log output reached **200–300 identical WARN lines
in a 2-second window**, masking all real events.

### Fix (commit `d8ac235` — 2026-04-07)

- **Rate-limited WARN** to once per 5 minutes per source IP — stops log flooding
- **`record_violation()` on every rejection** — peer is auto-banned after 3 rejections
  (1 min), 5 rejections (5 min), 10 rejections (permanent)
- **Coordinated /24 Sybil auto-detection** — if ≥5 unique IPs from the same /24 subnet
  each claim the same outpoint within 60 seconds, the entire /24 is automatically
  subnet-banned. The Sybil tracker uses a `DashMap<outpoint|subnet, Vec<(ip, timestamp)>>`
  with a 60-second sliding window
- **`bansubnet=` config option** — operators can statically block entire CIDR ranges in
  `time.conf` (e.g., `bansubnet=154.217.246.0/24`); enforced at TCP accept, before handshake

**Code references:**
- `src/network/message_handler.rs` — `CONFLICT_WARN` static rate limiter + Sybil tracker
- `src/network/blacklist.rs` — `add_subnet_ban()`, `in_banned_subnet()`, `subnet_ban_count()`
- `src/config.rs` — `blacklisted_subnets`; `bansubnet=` parser
- `src/network/server.rs` — subnet init loop; TCP-level ban enforcement

---

## Attack Vector 11: Pre-Handshake Prober

**Observed:** April 5–7, 2026 — all nodes  
**Severity:** Low-Medium — resource exhaustion + fingerprinting

### Description

Nodes (principally `154.217.246.33`, `43.129.27.42`, `8.218.124.20`,
`39.174.152.101`, `104.28.165.55`) established TCP connections and immediately sent
protocol data **before completing the handshake exchange**. This is a probing
technique to fingerprint the daemon version and a resource exhaustion vector
(forces connection setup + read loop spin for each probe).

The daemon correctly detected the early message and closed the connection, but
recorded no violation — the prober could reconnect every 30 seconds indefinitely.

```
⚠️  154.217.246.33:59680 sent message before handshake - closing connection
```

### Fix (commit `948041f` — 2026-04-07)

`blacklist.record_violation()` is now called immediately on every pre-handshake
message, in addition to the existing AI detector path. Five violations produce a
5-minute ban; ten produce a permanent ban.

**Code references:**
- `src/network/server.rs` — pre-handshake message handler
- `src/ai/attack_detector.rs` — `record_pre_handshake_violation()`; ≥10 → `BlockPeer`

---

## Attack Vector 12: Oversized Frame Header (Memory Exhaustion / Trivial DoS)

**Observed:** April 7, 2026 — LW-Michigan2  
**Severity:** Low (currently caught) — but previously free to repeat

### Description

A peer sent a TCP frame with a 4-byte length header claiming a body of
**2,823,396,163 bytes (~2.8 GB)**. Only 4 bytes need to be transmitted to trigger
the check. The daemon correctly rejected the frame and disconnected, but recorded
**zero violation** — the attacker could reconnect and repeat at no cost.

```
Connection from 188.166.243.108:60880 ended: Frame too large: 2823396163 bytes (max: 8388608)
```

*Note:* In this specific instance `188.166.243.108` is the operator's own Silver
node running an outdated binary with a serialization bug — not a malicious actor.
The fix is still correct: whitelisted IPs bypass the blacklist check.

### Fix (commit pending in `src/network/server.rs` — 2026-04-07)

`record_violation()` is now called whenever the `Err(e)` read-loop branch fires with
an error message containing `"Frame too large"`. After 3 oversized frames the peer is
temporarily banned; after 10, permanently.

**Code references:**
- `src/network/server.rs` — `Err(e)` branch in the message read loop
- `src/network/wire.rs` — `read_message()`: `MAX_FRAME_SIZE = 8 MB` enforcement

---

## Attack Vector 13: UTXO Lock Flood

**Observed:** April 5–7, 2026  
**Severity:** Medium — UTXO manager resource exhaustion

### Description

A peer sent an abnormally high volume of `UTXOStateUpdate(Locked)` messages for the
same transaction — far exceeding the number of inputs any legitimate transaction
would have. A normal TX with N inputs produces exactly N lock messages. The flood
forced repeated `lock_collateral` calls into the UTXO manager, exhausting resources
and generating excessive log output.

### Fix (commit `3c8bc59` — 2026-04-07)

- Per-connection per-TX lock counter (`peer_tx_lock_counts` HashMap), capped at 50
- Excess lock messages trigger `record_utxo_lock_flood()` in the AI attack detector
- "Applied UTXO lock" log lines downgraded INFO → DEBUG

**Code references:**
- `src/network/server.rs` — `peer_tx_lock_counts`; `MAX_UTXO_LOCKS_PER_TX = 50`
- `src/ai/attack_detector.rs` — `UtxoLockFlood` attack type; `record_utxo_lock_flood()`

---

## Attack Vector 14: V4 Eviction Oscillation (Free-Tier Re-squatting)

**Observed:** April 7, 2026 — LW-Michigan2  
**Severity:** Medium — collateral registration repeatedly stolen and reclaimed

### Description

After a legitimate node uses a V4 collateral proof to evict a free-tier squatter,
the squatter (or a confederate on the same Sybil subnet) immediately re-registers
for the same outpoint via the "free-tier IP migration" path from a different IP.
This creates a loop:

1. `154.217.246.19` squats `96d12d31...` (registered to `188.166.243.108`)
2. `188.166.243.108` presents V4 proof → squatter evicted ✅
3. V4 eviction storm cooldown prevents the owner from immediately reclaiming if a
   second V4 attempt arrives within 60 seconds
4. `69.167.169.81` (confederate) migrates the same outpoint via a free-tier gossip
   announcement — re-squats the registration
5. Cycle repeats

```
✅ V4 collateral proof verified: evicting squatter 154.217.246.19 → 188.166.243.108
🛡️ V4 eviction storm blocked: 154.217.246.19 → 154.217.246.194 — cooldown active
🔄 Free-tier IP migration: 96d12d31... moving from 154.217.246.19 to 69.167.169.81
```

### Current Mitigation

- Subnet ban of `154.217.246.0/24` (now auto-triggered by Attack Vector 10 fix) stops
  the cycle at the TCP level once ≥5 Sybil IPs are detected
- Free-tier claim rejections now accumulate violations → persistent attackers get
  permanently banned within a few cycles

### Remaining Gap

The oscillation can still occur before the subnet auto-ban threshold is reached, or
if the attacking IPs span multiple /24 blocks.

### Recommendation

Implement a **post-eviction re-registration delay** per outpoint: after a V4-proof
eviction removes a squatter, that squatter IP and any IP without a valid V4 proof
should be barred from claiming the same outpoint for N minutes (e.g., 10 minutes).
This is distinct from the V4 eviction storm cooldown (which rate-limits the
*legitimate owner's* re-eviction attempts) and would specifically penalise the
squatter side of the loop.

---

## Commits Ledger (April 7, 2026)

| Commit | Changes |
|--------|---------|
| `3c8bc59` | UTXO lock flood auto-ban (Attack Vector 13) |
| `948041f` | Pre-handshake prober direct blacklist violation (Attack Vector 11 → AV16); collateral hijacker severe violation on `CollateralAlreadyLocked` |
| `d8ac235` | Registry conflict rate-limit + violation recording + Sybil /24 auto-ban + `bansubnet=` config option (Attack Vectors 10, 14 partial) |
| `f33de46` | Oversized frame header violation recording (Attack Vector 12) |
| `1b9bf31` | Post-eviction lockout 600s (Attack Vector 14) |
| `f9e8e7c` | UTXO state-aware reconciliation: state discriminant in hash, UTXOStateResponse handler, post-reconciliation state query (Attack Vector 18 fix) |


Both eviction sites in `message_handler.rs` (UTXOManager-locked path and
registry-only path) now check `is_local_squatter` before the Tier 2 `can_evict`
branch. If the current holder is the local node, the Tier 2 eviction is rejected
regardless of address match, and a V4 cryptographic proof is required to displace
the local node.

```rust
// message_handler.rs — Tier 2 path
let is_local_squatter = context.node_masternode_address
    .as_deref()
    .map(|local| local == squatter_ip)
    .unwrap_or(false);
if is_local_squatter {
    warn!("🛡️ Blocked Tier 2 eviction attack — V4 proof required to displace local node");
    false // can_evict = false
} else { ... }
```

---

## Attack Vector 15: Pre-Handshake Connection Failure Triggers Masternode Deregistration

### Severity: Medium — Legitimate masternodes deregistered by failed connection attempts

### Description

When the TCP server accepted a connection and the connection was later closed,
`mark_inactive_on_disconnect(peer_ip)` was called unconditionally in the cleanup
path. This fires even if the peer closed the connection immediately after
connecting — before any handshake message was exchanged.

An attacker can open a TCP connection to port 24000, immediately close it, and
trigger `mark_inactive_on_disconnect` for whatever IP they connected from. If a
legitimate masternode was registered under that IP in the local registry, its
`is_active` flag is set to `false` — excluding it from reward eligibility until
the next gossip refresh cycle (up to 5 minutes).

**Observed in production:** Chinese Alibaba Cloud IPs (`154.217.246.x`) were
repeatedly connecting and disconnecting from port 24000 before handshake, causing
legitimate masternodes sharing those subnet ranges to flicker inactive.

### Fix (commit `89bd02d` — 2026-04-06)

In `server.rs`, `mark_inactive_on_disconnect` is now gated on a `handshake_done`
boolean that is set to `true` only after the `Version`/`Verack` handshake
completes successfully. Connections that drop before handshake completion are
silently closed without modifying registry state.

```rust
// server.rs — connection cleanup
if handshake_done {
    registry.mark_inactive_on_disconnect(&peer_ip).await;
}
// else: pre-handshake failure — no registry side effects
```

---

## Attack Vector 16: V4 Gossip Eviction of Local Node

### Severity: Critical — Removes local node from registry with cryptographic cover

### Description

Attack Vector 9 added Tier 2 protection for the local node, but **Tier 1 (V4
cryptographic proof)** had `can_evict = true` unconditionally at both eviction
sites. The code comment even says "Require Tier 1 (V4 cryptographic proof) to
displace the local node" in the Tier 2 block — but then Tier 1 itself had no
local-node guard.

An attacker who knows the local node's collateral outpoint (public on-chain) can:

1. Sign `TIME_COLLATERAL_CLAIM:<txid>:<vout>` with their own `masternodeprivkey`
2. Set `reward_address = victim's collateral UTXO address`
3. Broadcast a V4 announcement claiming the victim's collateral

Step 3 passes the V4 proof check (signature is valid for the attacker's key) and
passes the address check (if the attacker sets `reward_address` to the victim's
on-chain address). The eviction fires: `unregister(local_IP)` removes the local
node's entry from `masternodes`. `get_local_masternode()` then returns `None`.

**Observed effect (2026-04-06 18:31):**

- `getwalletinfo` returned `{"code": -4, "message": "Node is not configured as a
  masternode"}`
- Dashboard showed "Not a Masternode"
- Consensus signing key still present, but registry entry gone → node excluded from
  reward eligibility and dashboard became non-functional

Note: the RPC server was healthy and responding correctly throughout the attack —
the error code `-4` is a JSON-RPC application error, not a connectivity failure.

### Fix (commit `2d842f6` — 2026-04-06)

Both V4 eviction sites now check `is_local_squatter` before `can_evict = true`
for Tier 1, identical to the Tier 2 guard:

```rust
// message_handler.rs — Tier 1 (V4) path, both eviction sites
let is_local_squatter = context.node_masternode_address
    .as_deref()
    .map(|local| local == squatter_ip)
    .unwrap_or(false);
if is_local_squatter {
    warn!("🛡️ Blocked V4 eviction of local node {} by {} — use on-chain MasternodeReg",
          squatter_ip, peer_ip);
    false // can_evict = false
} else {
    // rate-limit check (see Attack Vector 12)
    ...
}
```

The local node can never be displaced via gossip regardless of tier or proof type.
On-chain `MasternodeReg` is the only mechanism for transferring collateral ownership.

---

## Attack Vector 17: Multi-Claimant V4 Proof Storm (Infinite Eviction Loop)

### Severity: High — DoS; 400+ registry operations/second; starves CPU and logging

### Description

When multiple nodes simultaneously hold valid V4 proofs for the same collateral
outpoint (e.g., due to a compromised private key, misconfigured nodes sharing a
key, or an operator migrating a node without deregistering the old one), they
create an infinite eviction cycle:

1. Node A's V4 proof evicts Node B → `unregister(B)` clears the `collateral_anchor`
2. Node B's V4 proof evicts Node A → `unregister(A)` clears the `collateral_anchor`
3. Node C's V4 proof evicts Node B → repeat indefinitely

Each iteration:
- Acquires the `masternodes` write lock
- Writes to sled (anchor removal)
- Calls `unlock_collateral` + `lock_collateral` on the UTXOManager
- Emits multiple `INFO` log lines

**Observed in production (2026-04-06 18:31:33–34):**

Three IPs (`64.91.241.10`, `50.28.106.227`, `69.167.168.176`) produced 435 log
lines within 2 seconds for collateral `926b2fb0...5c4:0`. The cycle ran until
the connections dropped or the process was restarted.

**Why the anchor does not prevent cycling:**

`unregister()` explicitly removes the `collateral_anchor` from sled so that the
evicted node can re-register with different collateral in future. This is correct
for normal deregistration but makes V4 eviction self-defeating: each successful
eviction enables the next one.

### Fix (commit `2d842f6` — 2026-04-06)

A per-outpoint V4 eviction cooldown is maintained in a process-lifetime
`DashMap<outpoint_str, Instant>`. After a V4 eviction fires for an outpoint, any
further V4 evictions for that same outpoint are blocked for `V4_EVICTION_COOLDOWN_SECS = 60`
seconds. The cooldown is stamped at eviction time, not at announcement time, so
the first legitimate eviction always goes through.

```rust
// message_handler.rs — before can_evict = true for Tier 1
let outpoint_key = outpoint.to_string();
let within_cooldown = v4_eviction_cooldown()
    .get(&outpoint_key)
    .map(|t| t.elapsed().as_secs() < V4_EVICTION_COOLDOWN_SECS)
    .unwrap_or(false);
if within_cooldown {
    warn!("🛡️ V4 eviction storm blocked for {} — cooldown active", outpoint);
    return false; // can_evict = false
}
// On eviction:
v4_eviction_cooldown().insert(outpoint.to_string(), Instant::now());
```

Storm-warning log messages are suppressed after the first hit (30-second warning
cooldown per outpoint) to prevent secondary log flooding from the blocked
announcements themselves.

**Combined effect with Attack Vector 11 fix:**

Because the local node can no longer be evicted at all (AV16), the local node is
immune to V4 storms targeting its own collateral. The cooldown (AV17) protects
the rest of the registry from storms targeting other nodes' collateral.

---

## Attack Vector 18: Invalid Block Flood (Crafted Reward Distribution)

**Observed:** April 7, 2026 — LW-Michigan (blocks 899+)  
**Severity:** High — causes 30-second sync stall on every block slot

### Description

The `154.217.246.0/24` Sybil subnet fabricated and broadcast block 899 with a
crafted reward distribution: `TIME158jRWhqLzgP8GfGqgUN2zpmCMqQ4VNpxR` received
5,500,000,000 satoshis (55 TIME), exceeding the maximum Gold tier pool of
2,500,000,000 (25 TIME) and belonging to no registered masternode.

Our node correctly rejected every copy (validation at `blockchain.rs:6393-6399`).
However, because old-binary legitimate nodes (`50.28.104.50`, `50.28.107.33`,
`188.166.243.108`, `69.167.169.81`, `64.118.152.210`, `64.118.153.4`, `188.26.80.38`)
had already accepted the crafted block, they propagated it to our node as the
network consensus. 16+ peers all served the same invalid block 899 in the same
sync round, causing our node to time out (30 seconds) before finding valid block
899 from `64.91.224.76`.

**Critical secondary failure:** The reward-hijacking block ban code (`message_handler.rs` ~5253)
matched on `"unique reward recipient"` and `"reward-hijacking"` but NOT on
`"unknown recipient"` or `"exceeds max tier pool"` — the actual error strings
produced by this specific block. As a result, every peer sending the crafted block
fell through to `skipped += 1` with **no violation recorded**. The peers could
repeat the attack indefinitely at zero cost.

### Attack Timeline (block 899)

```
05:52:10 — Sync coordinator: consensus at height 899 (ours: 898)
05:52:10-21 — 16 peers serve invalid block (unknown recipient, exceeds pool)
05:52:21 — 30s timeout fires: "No progress after parallel sync"
05:52:21 — AI fallback selects 154.217.246.33 (Sybil!) as "best peer"
05:52:30 — Valid block 899 obtained from 64.91.224.76 (legitimate)
```

### Fix (commit `TBD` — 2026-04-07)

Added `"unknown recipient"` and `"exceeds max tier pool"` to the reward-hijacking
match arm in `message_handler.rs`. Peers sending such blocks are now permanently
banned immediately, same as `"unique reward recipient"` violations:

```rust
Err(e)
    if e.contains("unique reward recipient")
        || e.contains("reward-hijacking")
        || e.contains("reward_hijack")
        || e.contains("under-subscribed genesis")
        || e.contains("unknown recipient")          // ← NEW
        || e.contains("exceeds max tier pool") =>   // ← NEW
```

### Remaining Gap

Old-binary nodes that accepted the crafted block will continue to propagate it
until they upgrade. The block stall window can be reduced but not eliminated until
all peers run validation-current binaries.

---

## Attack Vector 19: SNI False-Flag / Log Poisoning

**Observed:** April 7, 2026 — LW-Michigan, Arizona  
**Severity:** Low (operational confusion) — no direct consensus impact

### Description

An attacker presents a *friendly* node's IP address as the TLS SNI hostname in
their ClientHello, causing the victim node's logs to show that IP as the source
of bad TLS behavior. This is not an RFC 6066 compliance issue from a legitimate
node — it is a deliberate false-flag technique.

Example (Arizona logs, April 7 2026):

```
WARN Illegal SNI extension: ignoring IP address presented as hostname (36392e3136372e3136382e313736)
```

`36392e3136372e3136382e313736` hex-decodes to ASCII `69.167.168.176` — which is
the operator's own Michigan node, not the attacker.

The actual TCP source IP of these connections is from the `154.217.246.0/24`
attacker subnet. The attacker sets the SNI field to `69.167.168.176` to:

1. Confuse the node operator into thinking their own Michigan node is misbehaving
2. Potentially trick naive ban logic that keys on SNI value instead of source IP
3. Pollute logs and make root-cause analysis harder during an active attack

**Why it doesn't work (ban attribution is correct):**

`record_violation()` and `record_tls_failure()` both use `ip` — the actual TCP
source IP from `TcpListener::accept()` — NOT the SNI value. A connection that
fails TLS will accumulate violations against the real attacker IP, not the spoofed
SNI hostname. `69.167.168.176` will never appear in the blacklist as a result of
these attacks.

The SNI warning itself comes from `rustls` / `tokio-rustls` internals and cannot
be suppressed without patching the crate.

### Mitigation

No code changes required. To verify your own node is not incorrectly banned,
run `time-cli getblacklist` (added commit `a4d7daa` — 2026-04-07) and confirm the
friendly IP does not appear in the `permanent` or `temporary` sections.

Log noise can be filtered: `journalctl -u timed | grep -v "Illegal SNI"`

---

## Attack Vector 20: Address-Match Stalemate (Dual Claimants on Same UTXO Address)

**Observed:** April 7, 2026 — LW-Michigan (outpoint `0d16a18c...`)  
**Severity:** Low — log spam; registration ownership unclear but blocks work

### Description

Two paid-tier Silver nodes (`188.26.80.38` and `50.28.104.50`) claim the same
collateral outpoint `0d16a18cc319a1bf37a08c52dd26c1ab9572a1bc464fafb5d478949d2dc04b75:0`.
Both nodes' wallet addresses match the on-chain UTXO address `TIME1LeGqigKspRreyGBdSJYuDyz7NFyAZNYtY`.

In the same gossip round:
1. `188.26.80.38` gets "Collateral ownership verified via UTXO address" → evicts `50.28.104.50`
2. `50.28.104.50` gets "Collateral ownership verified via UTXO address" → evicts `188.26.80.38`
3. Both also trigger "Blocked wallet-match eviction of paid-tier" in the registry path

```
✅ Collateral ownership verified via UTXO address for 188.26.80.38 (outpoint 0d16a18c...): evicting squatter 50.28.104.50
🛡️ Blocked wallet-match eviction of paid-tier 188.26.80.38 by 50.28.104.50 (claimed tier: Silver)...
✅ Collateral ownership verified via UTXO address for 50.28.104.50 (outpoint 0d16a18c...): evicting squatter 188.26.80.38
🛡️ Blocked wallet-match eviction of paid-tier 50.28.104.50 by 188.26.80.38 (claimed tier: Silver)...
```

The UTXO-path and registry-path disagree: UTXO path allows the eviction (both
addresses match), registry path blocks it (paid-tier protection fires). The
result oscillates each gossip round and generates repeated log warnings.

### Root Cause

Two separate nodes were legitimately started with the same `masternode.conf`
collateral outpoint (duplicate config, hardware migration, or attempted transfer
without on-chain re-registration). The UTXO address match is ambiguous — both
are "the owner" by the on-chain evidence alone.

### Fix

When both the current holder AND the challenger are paid-tier nodes AND both
have matching UTXO address ownership, the UTXO-path eviction should be blocked
(same as the registry-path already does). The first registrant's claim is
authoritative until an on-chain `MasternodeReg` re-registers.

**Partially mitigated**: registry path correctly blocks the eviction. Block production
is unaffected since both nodes are registered during oscillation. The issue is log
noise, not a consensus failure.

---

## Attack Vector 22: Ghost Connection OOM (Distributed SNI Flood)

**Observed:** April 7, 2026 — all nodes (Michigan, Arizona)
**Severity:** Critical — crashes daemon process via OOM SIGKILL every ~12 minutes

### Description

A coordinated botnet sends ~10 TLS connections per second from distributed IPs,
all presenting the victim's own IP address as the TLS SNI hostname (encoded as
hex in the SNI extension, e.g., `35302e32382e3130342e3530` = ASCII `50.28.104.50`).

Each connection:
1. Completes TLS successfully (rustls warns but does not reject IP-as-SNI)
2. Never sends a `Version`/`Verack` handshake message
3. Holds a tokio `handle_peer` future for 10 seconds (pre-handshake timeout)

With ~10 connections/second × 10-second hold time = **~100 concurrent futures**,
each holding ~200 KB of TLS state + TCP buffer. This produces 20 MB of live task
memory per 10-second window, growing continuously until the OS OOM-killer fires.

**Compound crash mechanism (three simultaneous vectors):**

1. SNI ghost flood: ~10/sec × 10s = ~100 concurrent futures
2. PHASE3 reconnect loop: 15 banned IPs × full TCP+TLS before rejection every 30s
   (see AV23 below)
3. Synchronized disconnect storm: 7–10 nodes from `154.217.246.x` disconnect
   simultaneously every ~60s, triggering immediate reconnect storms

All three vectors fire together, causing memory to spike from ~400 MB to OOM
within approximately 10–12 minutes.

**Observed crash log pattern:**
```
WARN  Illegal SNI hostname presented: 35302e32382e3130342e3530  (hex-encoded IP)
... [~100 such lines over 10 seconds, then process killed by OOM]
```

**Watchdog impact:** Before this fix, `time-cli masternodestatus` could take up to
60 seconds to fail when the daemon was dead (TCP + TLS to dead daemon timeout).
With `FAIL_THRESHOLD=1` (old default) the watchdog was restarting within 1 check;
with a 60s RPC timeout stall, this appeared as a 12-minute outage cycle.

### Fixes

**Commit `2778693` — 2026-04-07:**
- `timed.service`: `MemoryMax=3G`, `MemoryHigh=2G`, `LimitNPROC=8192`
- Hard memory ceiling prevents kernel OOM from killing other system processes;
  systemd will restart the daemon if the limit is breached

**Commit `1affdfc` — 2026-04-07:**
- `mn-watchdog.sh`: Added `--rpc-timeout N` flag (default 8s) wrapping all
  `time-cli` calls with `timeout "$RPC_TIMEOUT"` to prevent 60s stalls
- `FAIL_THRESHOLD` default changed from 1 to 3 consecutive failures before restart
  (prevents thrashing on transient RPC errors)

**Commit `a028b52` — 2026-04-07:**
- `server.rs`: Per-/24 subnet accept rate limiter — if >20 connections/min arrive
  from the same /24 prefix, new connections are dropped before TLS.  
  Implemented as `DashMap<String, VecDeque<Instant>>` in the accept loop.
- `attack_detector.rs`: `record_tls_failure()` — ≥5 TLS failures from same IP
  in 60s → `BlockPeer` (stops repeat offenders from individual IPs)

```rust
// server.rs — subnet rate limiter (accept loop, before TLS)
const MAX_SUBNET_CONNECTS_PER_MIN: usize = 20;
let subnet = ip_str.splitn(4, '.').take(3).join(".");  // first 3 octets
let reject = {
    let mut entry = subnet_accept_rate.entry(subnet).or_default();
    entry.retain(|t| now.duration_since(*t).as_secs() < 60);
    entry.push_back(now);
    entry.len() > MAX_SUBNET_CONNECTS_PER_MIN
};
if reject { drop(stream); continue; }
```

---

## Attack Vector 23: PHASE3 Reconnect Loop to Banned Peers

**Observed:** April 7, 2026 — all nodes
**Severity:** High — wastes ~15 tokio tasks every 30 seconds connecting to known-banned IPs

### Description

The PHASE3 outbound connection loop (`client.rs`) checks all registered masternodes
and peers every 30 seconds to maintain full connectivity. The `should_skip()`
closure checked only:
- Self-connection (own IP)
- Static `blacklisted_peers` config set
- Already-connected state

It did **not** check `res.ip_blacklist` — the live `Arc<RwLock<IPBlacklist>>`
maintained by the AI enforcement loop. When the AI or manual config banned a
subnet (e.g., `154.217.246.0/24`), PHASE3 still attempted full TCP + TLS handshakes
to all ~15 IPs on that subnet before the first message was rejected:

```
[PHASE3-MN] Connected to peer: 154.217.246.34:24000
[PHASE3-MN] REJECTING message from blacklisted peer 154.217.246.34: Subnet banned
```

Each wasted connection consumed a tokio task, TLS state (~200 KB), and ~1–2 seconds
of wall-clock time. With the ghost connection flood happening simultaneously, these
wasted tasks contributed directly to the OOM condition.

### Fix (commit `a028b52` — 2026-04-07)

Both PHASE3-MN and PHASE3-PEER loops now check `ip_blacklist.write().await.is_blacklisted()`
before calling `mark_connecting`. Banned IPs are skipped at no cost — no TCP socket
opened, no TLS round-trip, no tokio task spawned.

```rust
// client.rs — PHASE3 blacklist check (both MN and PEER loops)
if let Some(ref bl) = res.ip_blacklist {
    if let Ok(parsed_ip) = ip.parse::<IpAddr>() {
        if bl.write().await.is_blacklisted(parsed_ip).is_some() {
            tracing::debug!("⏭️  [PHASE3-MN] Skipping {} (blacklisted)", ip);
            continue;
        }
    }
}
```

(`write()` is required because `is_blacklisted` takes `&mut self` to lazily expire
timed bans.)

---

## Attack Vector 24: IP Cycling / Collateral Migration Back-and-Forth

**Observed:** April 7, 2026 — all nodes
**Severity:** High — causes registry churn, disrupts reward attribution, triggers stale-collateral unlock storms

### Description

Four collateral outpoints were observed cycling between IP pairs on an exact 60-second
cadence — matching the old `MIGRATION_COOLDOWN_SECS = 60`:

| Outpoint (prefix) | IP pair |
|-------------------|---------|
| `50911bd...` | `154.217.246.34` ↔ `124.70.167.62` |
| `f52a81...`  | `154.217.246.111` ↔ `154.217.246.86` |
| `926b2f...`  | `133.18.180.117` ↔ `43.119.35.195` |
| `95f1b8...`  | `69.167.169.81` ↔ `47.82.236.153` |

Each flip:
1. Triggers "Unlocked 3 stale collateral(s)" in the UTXOManager
2. Fires a registry `add`/`remove` write to sled
3. Re-gossips the new registration to all connected peers
4. Temporarily removes the legitimate holder from reward eligibility

With 4 outpoints cycling every 60 seconds the registry receives 4 writes/sec
minimum, flooding the sled WAL and generating excessive log output.

**Attack goal:** Keep legitimate nodes flickering between active and inactive states,
disrupting reward distribution and generating CPU/IO load that compounds the AV22
ghost connection OOM.

### Fixes (commit `a028b52` — 2026-04-07)

**Cooldown raised 60s → 300s:**

```rust
// masternode_registry.rs
const MIGRATION_COOLDOWN_SECS: u64 = 300;  // was 60
```

Reduces cycling frequency by 5× immediately. Attackers can no longer flip on every
block slot.

**Back-and-forth cycling detection (10-minute lockout):**

New field `collateral_migration_from: Arc<DashMap<String, String>>` tracks the
source IP of the most recent accepted migration per outpoint. Before accepting any
new migration:

1. Look up the previous-from IP for this outpoint
2. If the incoming IP matches the previous-from IP AND the last migration was within
   `CYCLING_LOCKOUT_SECS = 600`, reject with `RegistryError::InvalidCollateral`

```rust
// masternode_registry.rs — cycling detection
if let Some(prev_from) = self.collateral_migration_from.get(&outpoint_key) {
    if prev_from.as_str() == incoming_ip {
        if now.saturating_sub(*last_migrated) < CYCLING_LOCKOUT_SECS {
            warn!("🛡️ IP cycling rejected (AV3): {} tried to move {} back to {} \
                   (came from there {}s ago, lockout {}s)",
                  masternode.address, outpoint, incoming_ip, ...);
            return Err(RegistryError::InvalidCollateral);
        }
    }
}
// Accept migration — record source IP for future cycling detection
self.collateral_migration_from.insert(outpoint_key.clone(), old_ip);
```

**Synchronized disconnect detection:**

`attack_detector.record_synchronized_disconnect(addr)` is now called in `handle_peer`
cleanup after `mark_inactive_on_disconnect`. If ≥5 nodes from the same /24 disconnect
within 30s, the AI emits `SynchronizedCycling` (AV3) and issues `BlockPeer` for the
specific offending IP. The **entire subnet is not banned automatically** — cloud
providers like Alibaba Cloud host both attacker and legitimate nodes on the same /24.
Operators who are certain a subnet is wholly hostile may add `bansubnet=x.x.x.0/24`
to `time.conf` for static enforcement.

### Fix (commit `45bb9ba` — 2026-04-09) — **Violations + disconnect for IP cycling**

**Gap in `a028b52`:** The AV24 cycling detection path returned
`Err(RegistryError::InvalidCollateral)` — the same generic error used for unrelated
collateral problems. The `InvalidCollateral` match arm in `message_handler.rs` only
logged "Failed to register masternode" with no violation recorded. An attacker cycling
an outpoint between two IPs could do so indefinitely at zero cost — no bans, no
disconnects.

**Fix:** Added `RegistryError::IpCyclingRejected` variant. The AV24 cycling detection
path now returns `IpCyclingRejected` instead of `InvalidCollateral`. Both
announcement handlers (`paid-tier` and `free-tier`) gained a new match arm:

```rust
RegistryError::IpCyclingRejected => {
    // 3-strike escalating ban: warn at 1, temp-ban at 3, permanent at 5/10
    let should_disconnect = blacklist.record_violation(&self.peer_ip, "IP cycling (AV3)");
    if should_disconnect {
        return Err(format!("DISCONNECT: IP cycling from {}", self.peer_ip));
    }
    Ok(None)
}
```

`record_violation` (minor) is used instead of `record_severe_violation` because a
single cycling attempt may be a legitimate node re-announcing while the lockout is
still active. Three strikes within the session trigger a temporary ban and disconnect.
Whitelisted peers are exempt (operator's own nodes are never disconnected for cycling).

---

## Commits Ledger (April 7, 2026 — Crash Recovery)

| Commit | Changes |
|--------|---------|
| `fd3f8b4` | Fee validation fix + TLS hardening (AV5, AV13 basis) |
| `2778693` | `timed.service` memory limits: `MemoryMax=3G`, `MemoryHigh=2G`, `LimitNPROC=8192` (AV22) |
| `1affdfc` | Watchdog RPC timeout wrapper + fail-threshold 3→2 default (AV22 watchdog) |
| `a028b52` | Subnet accept rate limiter; PHASE3 blacklist skip; AV24 cycling detection; BanSubnet enforcement loop; `record_synchronized_disconnect` + `record_tls_failure` hooks (AV22, AV23, AV24) |
| `651799c` | Switch synchronized disconnect mitigation from `BanSubnet` → `BlockPeer` to avoid collateral damage to legitimate cloud-hosted nodes (AV24 policy refinement) |

**Observed:** April 2026 — all nodes (balance queries return inconsistent results)  
**Severity:** Medium — wallet balance inconsistency across nodes

### Description

When connecting a wallet to different nodes, `getbalance` returns different values.
Root cause: the UTXO reconciliation hash (`calculate_utxo_set_hash`) included only
`(outpoint, value, script_pubkey)` but NOT `UTXOState`. Two nodes with the same
UTXOs but different states (e.g., one sees `SpentFinalized`, another sees `Unspent`
for the same outpoint) produced identical hashes → divergence was never detected →
reconciliation never triggered → balance discrepancies persisted indefinitely.

Additionally, `UTXOStateResponse` messages were silently dropped at the message
router instead of being applied as state updates.

### Fix (commit `f9e8e7c` — 2026-04-07)

1. `calculate_utxo_set_hash()` now includes a per-UTXO state discriminant (0–4)
   so any state divergence causes a hash mismatch and triggers reconciliation.
2. After UTXO-set reconciliation, the minority node sends `UTXOStateQuery` for all
   locally-Unspent outpoints; the majority peer responds with true states.
3. `apply_state_updates()` applies received states forward-only (never reverts
   spent → unspent) to prevent a malicious peer from fabricating spendable UTXOs.
4. `UTXOStateResponse` is now routed to `handle_utxo_state_response()` instead
   of being silently dropped.

---

## Attack Vector 25: Free-Tier Subnet Flooding (Registry OOM / PHASE3 Task Exhaustion)

**Observed:** April 7, 2026 (Michigan log `paste-1775596822700.txt`)
**Severity:** High — registry inflated to 65 nodes; PHASE3 maintained 15+ connections to one /24

### Description

The attacker registers large numbers of Free-tier masternodes from a single /24 subnet
(observed: 15+ nodes from `154.217.246.0/24`). Because `register()` had no per-subnet
cap, all nodes were accepted into the registry. When they disconnect simultaneously,
PHASE3 immediately reconnects all of them — each reconnect spawns a tokio task that
consumes ~200 KB of memory and holds a TLS session. With 15 nodes cycling at 65-second
intervals, the daemon accumulates a growing backlog of tasks and eventually OOMs.

**Root cause:** No per-/24 limit on Free-tier registrations. `mark_connecting()` in PHASE3
reconnects every node in `list_all()` regardless of how many are from the same subnet.

### Observed Indicators

```
[PHASE3-MN] Initiated 6 masternode reconnection(s) (65 registered)
Connected to masternode 154.217.246.48 ...
Connected to masternode 154.217.246.191 ...
... (15 distinct .246.x addresses in registry)
```

### Fix (commit `6170dee` — 2026-04-07)

1. **Registration cap** (`masternode_registry.rs::register()`):
   - New field `free_tier_subnet_counts: Arc<DashMap<String, u32>>`.
   - Helper `free_tier_subnet(ip)` extracts the /24 prefix (strips port, takes first 3 octets).
   - Before inserting a new Free-tier node: check count for its /24. If `>= 5`, reject with
     `RegistryError::InvalidCollateral`.
   - Increment count on successful insert; decrement in `mark_inactive_on_disconnect()` when
     a transient Free-tier node is removed.
   - On startup, populate counts from nodes loaded from disk so the cap is enforced immediately.

2. **PHASE3 reconnect cap** (`network/client.rs`):
   - `const MAX_FREE_TIER_RECONNECT_PER_SUBNET: usize = 3`
   - Before the reconnect loop, build `subnet_active_counts: HashMap<String, usize>` by
     iterating all registered masternodes and counting how many from each /24 are currently
     connected.
   - Inside the loop, skip reconnecting any Free-tier node whose /24 already has `>= 3` active
     connections. Increment the counter each time a new connection is spawned.
   - This limits tokio task accumulation even for nodes already in the registry (which bypass
     the registration cap because they were registered before the fix was deployed).

### Policy Revision (post-April 2026)

The per-/24 **registration cap** (max 5 Free-tier nodes per subnet) and the **PHASE3
reconnect cap** (max 3 active reconnects per subnet) described above were subsequently
**removed**.

**Rationale:** Legitimate operators may own an entire /24 subnet and run multiple Free-tier
masternodes from it. A blanket subnet cap incorrectly penalises honest operators while an
attacker using a VPS provider spread across many /24 prefixes can still evade it. The OOM
risk is better addressed by the following combination:

1. **Per-node misbehavior detection** — the AI attack detector already handles each of the
   individual bad behaviors that the subnet cap was trying to suppress in bulk:
   - Rapid cycling / collateral migration → AV3 back-and-forth lockout + AV26 migration
     frequency limit
   - Invalid vote signature spam → AV27 sliding-window violation
   - Unregistered voter spam → AV28 sliding-window violation
   - Sync loop flooding → AV11 `record_sync_flood()` rate-limit

2. **PHASE3 task limit retained** — OOM prevention is kept via an overall PHASE3 reconnect
   concurrency cap (not subnet-gated), so task accumulation is bounded regardless of how
   many Free-tier nodes are registered.

The `free_tier_subnet_counts` field and `MAX_FREE_TIER_RECONNECT_PER_SUBNET` constant were
removed from the codebase in the policy-reversal commit. Any individual node that
misbehaves is penalized by the per-node escalating ban in `IPBlacklist` rather than
triggering a blast-radius subnet ban.

---

## Attack Vector 26: Multi-Hop Collateral Pool Rotation (Back-and-Forth Evasion)

**Observed:** April 7, 2026 (Michigan log, lines 402-409)
**Severity:** High — evades AV3 (back-and-forth) detection; allows unlimited migration

### Description

AV3 (commit `a028b52`) detects back-and-forth cycling: A→B→A within 600 s is rejected
by checking if the incoming IP matches `collateral_migration_from` (the last source IP).
Attackers adapted by using rotation pools: A→B, then B→C, then C→D, then D→A.
Each hop looks like a fresh migration because the last source IP is always different.

**Observed in logs:**
```
50911bd: 154.217.246.34 → 124.70.167.62     (new source, evades AV3)
95f1b8:  47.82.236.153  → 69.167.169.81     (new source, evades AV3)
f52a81:  .86 → .86 blocked (AV3), then .86 → .111 → 64.91.248.55  (waited 300s, migrated via new hop)
926b2f:  133.18.180.117 → 43.119.35.195     (new source, evades AV3)
```

With a 300 s migration cooldown, an attacker using a 4-node pool can complete a full
rotation in 20 minutes, effectively re-squatting collateral outpoints indefinitely.

### Fix (commit `6170dee` — 2026-04-07)

New field `collateral_migration_counts: Arc<DashMap<String, (u32, u64)>>` in
`MasternodeRegistry`. Before accepting a migration (after AV3 back-and-forth check):

- Key: `"<txid>:<vout>"`, Value: `(count, window_start_unix_secs)`
- `MAX_MIGRATIONS_PER_WINDOW = 3`, `MIGRATION_WINDOW_SECS = 1800` (30 minutes)
- If the window is expired (`elapsed >= 1800s`), reset count to 0.
- If `count >= 3`, reject with `RegistryError::InvalidCollateral` and warn:
  `🛡️ [AV26] Migration flood rejected: ... (N migrations in Xs, max 3 per 1800s window)`.
- On successful migration, persist `(count + 1, window_start)` for the next check.

This limits any single outpoint to 3 legitimate IP migrations per 30 minutes,
which is more than enough for real operator IP changes but defeats rotation pools.

---

## Commits Ledger (April 7, 2026 — AV25/AV26 Fixes)

| Commit | Changes |
|--------|---------|
| `6170dee` | AV25: Free-tier per-/24 subnet cap (register + PHASE3); AV26: migration frequency limit; outbound synchronized-disconnect detection wired in client.rs spawn() |

---

## Attack Vector 27: Invalid Vote Signature Spam

**Observed:** April 7, 2026 — Michigan (22:10 logs, post-AV25 deployment)  
**Severity:** Medium — CPU waste, consensus noise from already-connected attacker IPs

### Description

Attacker IPs (observed: `154.217.246.86`) that were already connected before the AV25
registration cap was deployed continue sending `TimeVotePrepare` / `TimeVotePrecommit`
messages with **forged Ed25519 signatures** — signatures that pass the length check (64
bytes) but fail `public_key.verify()`. These messages are cheap to produce and arrive at
~1–3/second per attacking peer.

In `message_handler.rs`, the `verify_vote_signature()` function handles an invalid
signature with:

```rust
warn!("❌ Invalid vote signature from {}: {}", voter_id, e);
return Ok(false);
```

No violation is recorded. No disconnect is triggered. The peer remains connected
indefinitely and can sustain the flood for the lifetime of the TCP session.

### Current Behavior (un-fixed)

```
WARN ❌ [Inbound] Invalid PREPARE vote signature from TIME1...: signature error
WARN ❌ [Inbound] Invalid PREPARE vote signature from TIME1...: signature error
... (repeating every ~500ms from 154.217.246.86)
```

### Fix Implemented

A sliding-window counter `invalid_sig_vote_window` was added in `message_handler.rs`.
After **5 Ed25519 signature failures within a 30-second window** from the same peer IP,
`record_invalid_vote_sig_spam()` is called on the `AttackDetector`, which records a
violation in `IPBlacklist`. Structurally malformed votes (empty signature or wrong
length) still trigger an immediate violation without waiting for the threshold.

The `AttackDetector` exposes an `InvalidVoteSignatureSpam` attack type that maps to the
`RateLimitPeer` mitigation action, feeding the 30-second server enforcement loop.

**Status:** ✅ Fixed — `invalid_sig_vote_window` sliding window in `message_handler.rs`

---

## Attack Vector 28: Unregistered Voter Spam

**Observed:** April 7, 2026 — all nodes (post-AV25)  
**Severity:** Medium — registry lookup + crypto overhead per rejected vote; ~15 votes/sec wasted

### Description

After AV25 prevents new `154.217.246.x` nodes from registering, previously-connected
nodes from that subnet (which registered before the fix) continue relaying
`TimeVotePrepare` / `TimeVotePrecommit` messages using voter IDs for masternodes that
are not in the registry. The `verify_vote_signature()` unregistered-voter path fires:

```rust
let Some(info) = registry.get(voter_id).await else {
    warn!("❌ Rejecting vote from unknown/unregistered voter {}", voter_id);
    return Ok(false);
};
```

Again: no violation recorded, no disconnect. The attacker harvests registry-lookup
cycles (async DashMap read per message) at zero marginal cost.

**Why threshold must be lenient:** Votes are gossiped by connected peers on behalf of
remote masternodes. A legitimate well-connected peer may relay a vote from a node that
has just been deregistered. The threshold should be high enough to allow for transient
deregistrations (e.g., 10+ rejections per sliding window) before recording a violation.

### Fix Implemented

Track per-peer unregistered-vote count in `message_handler.rs`. After N rejections from
the same source IP within a window, call `record_violation()`. Suggested threshold:
10 unregistered votes within 60 seconds → 1 violation.

**Location:** `src/network/message_handler.rs` lines ~343-350

**Status:** ✅ Fixed — `unregistered_vote_window` sliding window in `message_handler.rs`;
10 unregistered-voter rejections within 60 seconds triggers one violation via
`record_unregistered_voter_spam()` on the `AttackDetector` (`UnregisteredVoterSpam`
→ `RateLimitPeer`).

---

## Bug Fix: Free-Tier Reward Rounding Dust → Chain Stall

**Observed:** April 7, 2026 — Arizona (block 997 re-proposal cycle)  
**Severity:** High — causes block validation mismatch on any block with ≥2 Free-tier recipients

### Description

In tier-based block production, integer division of the Free-tier pool among N nodes
produces rounding dust. Example: pool = 800,000,000 sat, 3 nodes → `266,666,666 × 3 =
799,999,998` → 2 sat dust. The old code subtracted this dust from the `block_reward`
header field:

```rust
let adjusted_reward = total_reward - rounding_dust;
// ... coinbase = adjusted_reward, block.header.block_reward = adjusted_reward
```

The validator independently computes `base + fees - treasury = total_reward` and
compares it to `block.header.block_reward`. Since `adjusted_reward ≠ total_reward`,
every block with any Free-tier dust was rejected with:

```
incorrect block_reward: expected 9500000000, got 9499999998
```

The block had to be re-proposed by another node (without the dust discrepancy) to advance.
Maximum dust = `MAX_FREE_TIER_RECIPIENTS - 1 = 24` satoshis (well within economic tolerance
but enough to break strict integer validation).

### Fix (commit `a05d483` — 2026-04-07)

Route rounding dust to the block producer's reward entry instead of burning it. Return
`total_reward` (the outer, pre-dust value) from the tier-based branch so the header
matches the validator's computation:

```rust
// blockchain.rs — tier-based reward distribution
if let Some(producer_entry) = rewards.first_mut() {
    producer_entry.entry.1 += rounding_dust; // give dust to producer
}
// return total_reward (not adjusted_reward) — matches validator's computation
total_reward
```

**Location:** `src/blockchain.rs` ~lines 3879-3901

---

## Operational Tool: getblacklist / unban / addwhitelist RPC + CLI

**Added:** April 7, 2026 (commit `a4d7daa`)

### Background

During active attacks it was impossible to inspect which IPs were actually banned,
because `getblacklist` returned only counts (not IP lists). Operators could not confirm
whether SNI false-flag attacks had incorrectly attributed bans to friendly nodes.

### Changes

**`IPBlacklist::list_bans()`** — new method returning:
- All permanent bans (IP, reason)
- All active temporary bans (IP, remaining seconds, reason)
- All subnet bans (CIDR, reason)
- All violation-count entries (IP, count), sorted descending

**`IPBlacklist::unban(ip)`** — removes an IP from permanent + temporary bans and
clears its violation count. Returns `true` if the IP was actually banned.

**`getblacklist` RPC** — now returns full lists under keys `permanent`, `temporary`,
`subnets`, `violations` plus a `summary` object with counts.

**`unban <ip>` RPC + CLI** — removes a specific IP from the ban list on demand.

**`addwhitelist <ip>` CLI** — was already an RPC, now wired into the CLI.

**Usage:**
```bash
# See all bans with reasons
./time-cli getblacklist

# Confirm 69.167.168.176 is NOT banned (SNI false-flag check)
./time-cli getblacklist | grep 69.167

# Unban a specific IP
./time-cli unban 154.217.246.86

# Whitelist a friendly node
./time-cli addwhitelist 69.167.168.176
```

---

## AV29 — Bitmap Positional Drift → `collateral_utxo_tier_map` False-Rejection

**Date observed:** April 8, 2026  
**Node affected:** LW-Michigan (`64.91.241.9`)  
**Status:** ✅ Fixed — commit `875b1d9`

### Observed behaviour

Michigan accepted blocks up to height 1018, then started rejecting every subsequent block from the majority chain with a "reward injection detected" false-positive. The node was stuck indefinitely, earning no rewards.

### Root cause

`validate_pool_distribution()` builds a `collateral_utxo_tier_map` to look up UTXO owner addresses for paid-tier masternode rewards. After `COLLATERAL_REWARD_ENFORCEMENT_HEIGHT=750`, rewards for paid-tier nodes (Gold/Silver/Bronze) are directed to the *UTXO owner address* of the collateral output — not the `reward_address` field in gossip state.

The bug: `collateral_utxo_tier_map` was built from the output of `get_active_from_bitmap()`, which decodes the free-tier activity bitmap using **IP-string-sorted order**. This sort is non-deterministic across peers with different gossip state (AV6 — bitmap positional drift). The decode was already producing a wrong node set. Legitimate paid-tier UTXO owner addresses were absent from the map and thus absent from `eligible`. Every valid block was rejected with:

```
validate_pool_distribution: reward injection detected — recipient X not in eligible set
```

### Why this stalled the chain

The majority of the network used slightly different gossip state, so only Michigan experienced the false-positive. All blocks from the majority chain (produced by correctly-paid nodes) were rejected. Michigan could not advance past 1018.

### Fix (`875b1d9`)

**Fix A:** Build `collateral_utxo_tier_map` from `get_all_paid_tier_infos()` — iterates **all known paid-tier masternodes** via `all_infos`, not the bitmap-decoded subset.

**Fix B:** In Step 5 of `validate_pool_distribution()`, before hard-failing on an unknown recipient, check whether the address appears in `all_infos` or `collateral_utxo_tier_map`. If it does, emit a `WARN` and accept the block (graceful bitmap drift handling).

### Detection signals

- Log line: `validate_pool_distribution: reward injection detected` when your own legitimate nodes' UTXO owner addresses are flagged.
- Node advances to a height but refuses to extend it, while other nodes at higher heights continue producing blocks.
- `time-cli getblockchaininfo` shows node height lagging the network by an increasing gap.

### Long-term fix needed

AV6 (bitmap positional drift) is the root cause. The bitmap should be keyed by collateral outpoints rather than IP-string-sorted positions — a consensus-breaking change requiring a coordinated network upgrade.

---

## AV30 — Genesis-Confirmed Deadlock (`BlockHashResponse` Routing Bug)

**Date observed:** April 8, 2026  
**Node affected:** LW-Michigan (`64.91.241.9`)  
**Status:** ⚠️ Code bug identified, fix needed in `message_handler.rs`

### Observed behaviour

After Michigan was patched (AV29 fix, commit `875b1d9`) and restarted, it synced blocks 1019–1023 but landed on a *minority fork* (block 1023 hash `5a3c360def4c89bf` instead of majority chain `f54c87ab9f62918e`). Michigan detects the fork on every incoming block from Michigan2 (`64.91.241.10`) and other whitelisted peers, but **never resolves it**:

```
🚫 Skipping fork resolution with 64.91.241.10 — peer not genesis-confirmed (likely old code)
```

This message repeats every ~1 second, indefinitely. Michigan earns no rewards while on the minority fork.

### Root cause (three-layer problem)

#### Layer 1 — In-memory genesis state lost on restart

`genesis_confirmed_peers` is an `Arc<RwLock<HashSet<String>>>` that lives entirely in RAM. Every daemon restart wipes it. After a restart, all peers — including long-trusted whitelisted nodes — must re-complete genesis verification before fork resolution is permitted.

#### Layer 2 — `BlockHashResponse` never reaches `send_and_await_response`

For **outbound** connections (Michigan → Michigan2), genesis confirmation is triggered lazily by `claim_genesis_check()` in `handle_chain_tip_response()`. It calls `verify_genesis_compatibility(peer_ip)`, which:

1. Sends `GetBlockHash(0)` to the peer via `send_and_await_response(peer_ip, request, Duration::from_secs(10))`.
2. Waits for the peer to respond with `BlockHashResponse { hash }`.
3. On match: calls `mark_genesis_confirmed()` — fork resolution unlocked.
4. On timeout: calls `release_genesis_check()` — stamps a **5-minute cooldown** on that peer.

The fatal defect: in `message_handler.rs`, the `NetworkMessage::BlockHashResponse { .. }` match arm simply returns `Ok(None)` — it **never calls `context.peer_registry.handle_response()`**. The oneshot channel waiting inside `send_and_await_response` never receives the response. The function **always times out** (100% of the time, by design of the routing bug).

#### Layer 3 — 5-minute cooldown compounds the problem

Every timeout stamps a 5-minute cooldown via `release_genesis_check()`. The fork is detected and logged every ~1 second. But genesis verification is blocked for 5 minutes. During those 5 minutes the node is trapped on the minority fork.

Even when the cooldown expires and a new check is attempted, it will time out again (Layer 2). The node is permanently deadlocked until the peer disconnects and reconnects (which calls `unregister_peer()`, clearing both confirmed state and the cooldown).

#### Contrast with inbound path (works correctly)

For **inbound** connections, `server.rs` sends `GetGenesisHash` (a different message type) immediately after handshake. The peer replies with `GenesisHashResponse(hash)`. This is handled by `handle_genesis_hash_response()` in the message handler, which calls `mark_genesis_confirmed()` directly — no use of `send_and_await_response`. This is why inbound peers show `✅ [Inbound] Genesis hash verified` almost immediately.

### Code locations

| Location | Issue |
|----------|-------|
| `src/network/message_handler.rs` ~line 895–903 | `BlockHashResponse` arm returns `Ok(None)` without calling `handle_response()` |
| `src/network/peer_connection_registry.rs` ~line 348–431 | `verify_genesis_compatibility()` — always times out due to the above |
| `src/network/peer_connection_registry.rs` ~line 454–473 | `release_genesis_check()` — 5-min cooldown stamped on every timeout |
| `src/network/message_handler.rs` ~line 5475–5488 | Fork resolution gate — checks `is_genesis_confirmed()` |

### Fix

In `message_handler.rs`, in the `BlockHashResponse` match arm, forward the message to the waiting oneshot channel before returning:

```rust
// Before (broken):
NetworkMessage::BlockHashResponse { .. } => {
    Ok(None)
}

// After (fixed):
NetworkMessage::BlockHashResponse { .. } => {
    context.peer_registry.handle_response(&self.peer_ip, message.clone()).await;
    Ok(None)
}
```

**Defense-in-depth (whitelist bypass):** Whitelisted peers (operator's own nodes) should skip the genesis-confirmed gate in fork resolution. If `is_whitelisted(peer_ip)` is true, allow fork resolution regardless of genesis-confirmed state. This prevents a code bug or transient network condition from permanently trapping an operator node on a minority fork.

### Detection signals

- Log pattern: `🚫 Skipping fork resolution with <ip> — peer not genesis-confirmed (likely old code)` repeating every ~1 second.
- The flagged IP is a known-good whitelisted node.
- `time-cli getblockchaininfo` shows your node at a lower height than the network.
- No log line `✅ Genesis hash verified with peer <ip>` for the affected outbound peers even after minutes of uptime.

### Exploitability

An attacker who can (a) get a victim node to restart (DoS, watchdog trip, etc.) and (b) inject a forked block before legitimate peers confirm genesis can permanently strand the victim on a minority fork with zero effort, requiring manual operator intervention to recover.

---

## AV31 — Fork Injection via Catch-Up Sync from Attacker Subnet

**Date observed:** April 8, 2026  
**Node affected:** LW-Michigan (`64.91.241.9`)  
**Status:** ⚠️ Partially mitigated (subnet ban); full fix pending

### Observed behaviour

After Michigan restarted with the AV29 fix, it synced blocks 1019–1023 from connected peers. It landed on block 1023 hash `5a3c360def4c89bf` — a minority fork. The majority chain's block 1023 is `f54c87ab9f62918e`. Michigan never recovered due to AV30 also triggering simultaneously.

### Root cause

During catch-up sync, a node requests missing blocks from **all connected peers concurrently**. Whichever peer responds first wins — the node accepts and validates that block, then caches it. If an attacker subnet controls the majority of the node's connections, attacker-supplied blocks arrive first and are accepted.

**Michigan's peer composition at the time of restart:**
- 6+ connections from attacker subnet `154.217.246.0/24` (no subnet ban on Michigan, unlike Michigan2)
- 2–3 connections from legitimate peers (Michigan2 + others)

The attacker had a 2:1 advantage in connection slots. Their forked block 1023 arrived before the legitimate version.

### Why this is dangerous in combination with AV30

Once a node accepts a forked block (AV31), it needs fork resolution to escape. But fork resolution is blocked by AV30 (genesis-confirmed deadlock). The two vectors compound each other into an **indefinite fork trap** requiring manual operator intervention.

### Mitigation (operational)

Before restarting a node that will perform catch-up sync, pre-ban the attacker subnet:

```bash
./time-cli bansubnet 154.217.246.0/24
```

The ban list is checked on new connections. Existing connections are not dropped unless a subsequent message triggers a violation. **Restart the daemon immediately after applying the subnet ban** so it comes up with attacker IPs excluded from connections before sync begins.

### Full fix (code change needed)

1. **Sync ordering**: During catch-up sync (`handle_get_blocks` / block request logic), prioritize responses from whitelisted peers. Treat non-whitelisted block responses as secondary candidates that are only accepted if they match a whitelisted peer's response — or if no whitelisted peer has responded within a timeout.
2. **Blacklist enforcement on inbound BlockResponse**: Reject `BlockResponse` messages from IPs that are on the subnet blacklist, even if they are not yet individually banned.
3. **Disconnect on catch-up mismatch**: If a whitelisted peer and a non-whitelisted peer supply different versions of the same block height during sync, immediately disconnect the non-whitelisted peer and record a violation.

### Detection signals

- Node height stalls immediately after restart at a block height that doesn't match the network.
- `time-cli getblockchaininfo` shows a block hash at the stall height that differs from majority nodes.
- Logs show the node initially syncing normally then stopping at a specific height.
- No "fork resolution" log lines despite connected peers being at higher heights (AV30 co-occurring).

---

## AV32 — Gossip Flood → Tokio Worker Starvation

**Date observed:** April 2026 — LW-Arizona, AB-HongKong  
**Status:** ✅ Fixed (commits `06e1481`–`5a51407`, April 2026)  
**Severity:** Critical — all 4 tokio workers blocked → RPC dead → watchdog restart → node loses masternode registration

### Observed behaviour

Nodes restarted by watchdog with no obvious crash. Before restart: ≥9 concurrent
Free-tier masternode gossip announcements arrive simultaneously. All RPC calls
timeout. Block production stops. After watchdog-triggered restart the node loses its
own masternode registration (because `mark_inactive_on_disconnect` runs on all peers
during shutdown, including local node's own entry).

On AB-HongKong: `50.28.106.227` flooding ~15 "Free-tier claim rejected" messages per
30s, and `133.18.180.117` flooding ~10 "IP cycling rejected" per 30s — both connected
indefinitely because the disconnect return value was discarded (see AV4 and AV24
fixes above).

### Root cause

`masternode_registry.rs` held a tokio `async RwLock` write lock (`masternodes.write().await`)
across multiple internal `.await` calls that resolved to blocking sled I/O via
`spawn_blocking`. On 4-worker tokio runtimes (typical VPS), this is catastrophic:

1. **`mark_inactive_on_disconnect` (free-tier path)**: called `self.db.remove()` — a
   synchronous sled write — directly on the tokio thread. 9 concurrent free-tier
   disconnects = 9 sync disk writes = all 4 workers saturated.

2. **`mark_inactive_on_disconnect` (paid-tier path)**: held `masternodes.write().await`
   while calling `broadcast_tx.read().await` — a nested async lock acquisition under a
   write lock. Any task waiting for `masternodes.read()` was blocked until the broadcast
   channel was also free.

3. **`cleanup_stale_reports`** (60s timer): held `masternodes.write().await` while
   calling `peer_registry.get_connected_peers().await` and
   `local_masternode_address.read().await` — two external async awaits under the
   write lock.

4. **`register_internal`**: held `masternodes.write().await` while calling
   `local_masternode_address.read().await`.

**Why UTXO-fetching inside the lock causes stalls:** `SledUtxoStorage::get_utxo()`
uses `tokio::task::spawn_blocking` internally — calling it inside any async write
lock is guaranteed to stall all readers of that lock until the blocking I/O completes.

### Fixes

**Write-behind sled channel** (`06e1481`): Added `SledWriteOp` enum, an
`UnboundedSender<SledWriteOp>`, and a background task draining the channel.
`sled_insert_bg()` and `sled_remove_bg()` queue writes without blocking. All direct
`self.db.remove()` / `self.db.insert()` calls inside write lock scope replaced with
these helpers.

**Pre-fetch async state before acquiring write lock** (commits `4c09d11`, `5a51407`):
- Free-tier disconnect: `sled_remove_bg` (no more sync write on tokio thread)
- Paid-tier disconnect: lookup broadcast sender, drop `masternodes.write()` before
  sending
- `cleanup_stale_reports`: pre-fetch `connected_peers` and `local_addr` before
  `masternodes.write().await`
- `register_internal`: pre-fetch `local_masternode_address` before write lock

**General rule:** Never call `.await` on anything involving sled I/O, external locks,
or channel I/O while holding `masternodes.write()`. All external state needed inside
the write lock must be fetched before acquiring it.

### Why the attack amplifies this bug

With the gossip disconnect fixes disabled (prior to `45bb9ba`), attackers could
maintain persistent connections flooding `MasternodeAnnounce` messages. Each rejected
announcement triggered registry error handling which accessed the write lock.
Combined with legitimate Free-tier disconnect storms from AV25 subnet flooding, the
number of concurrent write-lock acquisitions consistently exceeded the 4-worker limit.

### Detection signals

- Watchdog restarts every ~12 minutes without any daemon crash
- RPC starts timing out 30–90 seconds before watchdog fires
- `journalctl -u timed` shows 9+ "Masternode X removed on disconnect (transient Free-tier node)"
  lines in rapid succession before the RPC timeout window
- Block production log gap matches the worker stall duration exactly
- Node loses its own masternode registration after restart (sign that `mark_inactive_on_disconnect`
  ran on local entry during stall-triggered shutdown)

---

## AV37 — Registration Spam / Slot ID Exhaustion

**Date observed:** April 2026 — mainnet height 160  
**Status:** ✅ Fixed (v1.4.35, commit `268eaa9`, fork height 200)  
**Severity:** High — exhausts slot namespace; corrupts fairness-rotation bitmaps; floods registry

### Observed behaviour

250+ `MasternodeRegistration` transactions arrived within one second at height 160, all for
IPs that were already registered, each carrying a unique txid and a different wallet address.
The in-memory registry swelled; `assign_next_slot_id()` was called once per transaction,
burning sequential slot_id values. The DashMap retained only the last entry per IP, but the
orphaned slot_ids shifted every subsequent node's bit position in the sorted reward bitmap.

**Attacker IPs and registration counts (height 160):**
- `188.26.80.38` — 49 registrations, slot_ids 187900–188146 burned
- `50.28.104.50` — 29 registrations
- `64.91.241.10` — 23 registrations

### Root cause

`apply_masternode_registration()` idempotency guard checked `existing.registration_txid == registration_txid`.
A different txid for the same IP fell through to `assign_next_slot_id()`, treating it as a
brand-new node. There was no IP-level uniqueness check.

### Consensus impact of a naive fix

The bitmap is built by sorting masternodes by `slot_id` ascending. If the slot-reuse fix is
applied without a height gate, fresh nodes replaying from genesis compute `slot_id = 187900`
for `188.26.80.38` (first registration), while existing nodes have `slot_id = 188146` (last
registration). Different slot → different bit position → validators reject each other's blocks.

### Fix: Height-gated one-slot-per-IP rule

A fork height constant (`SLOT_UNIQUENESS_FORK_HEIGHT = 200`) gates the new behaviour:

- **Before height 200:** each re-registration with a new txid allocates a fresh slot_id
  (legacy behaviour). Replay of pre-fork history produces identical slot_ids on all nodes.
- **From height 200:** a re-registration with a different txid reuses the IP's existing
  slot_id. New IPs still receive a fresh slot.
- **Same-txid path** is fully idempotent at any height (unchanged).

All nodes — existing (sled state preserved) and fresh (replaying from genesis) — compute
identical bitmaps for every block, because:
1. Pre-fork blocks: all nodes use old rules → same slot_ids.
2. Post-fork blocks: all nodes use new rules → slot_ids stable on re-registration.

### Detection signals

- Burst of "Masternode registered on-chain" log lines at the same block height
- `assign_next_slot_id()` counter advancing hundreds of times in under one second
- Multiple distinct txids mapping to the same `node_address` in sled (`mnreg:{ip}` key)
- Reward bitmap length suddenly larger than the known active masternode count