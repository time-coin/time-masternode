# MASTERNODE PEER CONNECTION ANALYSIS - TIME COIN

## EXECUTIVE SUMMARY

The TIME Coin masternode network uses a **PYRAMID TOPOLOGY** with tier-based connection management:
- **Small Networks (≤20 nodes)**: Full mesh (ALL nodes connect to ALL others)
- **Large Networks (>20 nodes)**: Pyramid hierarchy (Gold → Silver → Bronze → Free)

Current issue: Nodes have varying connection counts (4-6 connections) despite supporting up to 50 peers max.
Root cause: Multiple limiting factors prevent full mesh achievement.

---

## 1. PEER DISCOVERY & CONNECTION INITIATION (src/main.rs)

### 1.1 Initial Peer Discovery
**Source:** src/peer_manager.rs and src/main.rs line ~794-801

**Discovery Flow:**
1. **PeerManager Initialization** (line 794):
   - Loads peers from persistent sled database
   - Calls discover_peers_from_server() to fetch from central API
   
2. **Central Server Discovery** (peer_manager.rs line 120):
   - Uses 
etwork_type.peer_discovery_url() (time-coin.io API endpoint)
   - Returns IPs without ports
   - Adds them as "candidates" (not yet verified)
   - Code: Stores in HashSet, adds to peer_info list with last_seen: 0

3. **Bootstrap Peers** (config.rs):
   - Hardcoded in ootstrap_peers config array
   - Used as fallback if peer discovery API fails
   - Included in initial connection list

### 1.2 Outbound Connection Establishment

**Main Loop:** src/network/client.rs line ~175-475

**THREE PHASES:**

#### **PHASE 1: Startup Pyramid Connections** (lines 175-341)
Runs ONCE on daemon startup:

`ust
// Line 225-308: Determine connection targets by tier
if total_masternodes <= FULL_MESH_THRESHOLD {  // Threshold = 20 nodes
    // Small network: connect to EVERYONE
    targets = [gold, silver, bronze, free all nodes]
} else {
    // Large network: tier-based targeting
    match our_tier {
        Gold => {
            // Gold: full mesh with all Gold + N Silver nodes
            targets = all_gold + silver[0..GOLD_SILVER_EXTRAS]  // GOLD_SILVER_EXTRAS = 3
        }
        Silver => {
            // Silver: all Gold (backbone) + lateral Silver peers
            targets = all_gold + silver[0..SILVER_LATERAL]  // SILVER_LATERAL = 4
        }
        Bronze => {
            // Bronze: N Silver upward + lateral Bronze
            targets = silver[0..BRONZE_UPWARD] + bronze[0..BRONZE_LATERAL]
            // BRONZE_UPWARD = 5, BRONZE_LATERAL = 3
        }
        Free => {
            // Free: N Bronze upward + 1 Silver fallback
            targets = bronze[0..FREE_UPWARD] + silver[0..1]
            // FREE_UPWARD = 5, falls back to Gold if no Bronze
        }
    }
}
`

**CRITICAL CODE (line 318-330):**
`ust
for ip in targets.iter().take(reserved_masternode_slots) {
    if should_skip(ip) { continue; }
    if !connection_manager.mark_connecting(ip) { continue; }
    res.spawn(ip.clone(), true);  // is_masternode=true
}
`

**KEY LIMIT:** eserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30)
- Default max_peers = 50
- Calculation: 50 * 40 / 100 = 20 slots reserved for masternodes
- Clamped to range [20, 30] → Result: **20 slots max**

#### **PHASE 2: Fill with Regular Peers** (lines 342-371)
After masternode connections, fill remaining slots with regular peers:

`ust
let available_slots = max_peers.saturating_sub(masternode_connections);
for ip in unique_peers.iter().take(available_slots) {
    if should_skip(ip) { continue; }
    if masternode_registry.get(ip).await.is_some() { continue; }  // Skip masternodes
    res.spawn(ip.clone(), false);
}
`

#### **PHASE 3: Periodic Discovery Loop** (lines 374-475)
Runs every 30 seconds (line 374):

`ust
const PEER_DISCOVERY_INTERVAL: Duration = Duration::from_secs(30);
loop {
    sleep(peer_discovery_interval).await;
    
    // Every iteration:
    // 1. Clean up stale "Connecting" states (stuck >30s)
    // 2. Count: outbound_count, inbound_count, active masternodes
    // 3. Calculate available_slots = max_peers - current_connections
    // 4. Sort regular peers by load (prefer less-loaded)
    // 5. Spawn connections for available slots
    
    // RECONNECTION LOGIC (lines 418-449):
    // - Skip masternodes (only retry on startup)
    // - Check AI advice for cooldown
    // - If 5+ consecutive failures → evict peer forever
    // - Otherwise → spawn based on AI recommendation
}
`

**CRITICAL CODE (lines 406-450):**
`ust
let mut unique_peers = dedup_peers(peer_manager.get_all_peers().await);
unique_peers.sort_by_key(|ip| peer_registry.get_peer_load(ip));  // Prefer less-loaded
for ip in unique_peers.iter().take(available_slots) {
    if should_skip(ip) { continue; }
    if masternode_registry.get(ip).await.is_some() { continue; }
    if connection_manager.is_reconnecting(ip) { continue; }
    
    // AI-based cooldown: skip unreliable peers
    const FORGET_THRESHOLD: u32 = 5;
    let failures = reconnection_ai.consecutive_failures_for(ip);
    if failures >= FORGET_THRESHOLD {
        peer_manager.remove_peer(ip).await;  // PERMANENT EVICTION
        continue;
    }
    
    let advice = reconnection_ai.get_reconnection_advice(ip, false);
    if !advice.should_attempt { continue; }  // Skip if AI says no
    
    res.spawn(ip.clone(), false);
}
`

---

## 2. CONNECTION LIMITS (src/network/)

### 2.1 Configuration (src/config.rs)
`ust
pub struct NetworkConfig {
    pub max_peers: u32,  // Default: 50
}
`

**User Config Override:** 	ime.conf key maxpeers or max_peers

### 2.2 ConnectionManager Hard Limits (src/network/connection_manager.rs lines 12-21)

`ust
const MAX_TOTAL_CONNECTIONS: usize = 125;           // Hard limit
const MAX_INBOUND_CONNECTIONS: usize = 100;         // Inbound only
const MAX_OUTBOUND_CONNECTIONS: usize = 25;         // Outbound only
const RESERVED_MASTERNODE_SLOTS: usize = 50;        // For whitelisted masternodes
const MAX_REGULAR_PEER_CONNECTIONS: usize = 75;     // Non-whitelisted slots
const MAX_CONNECTIONS_PER_IP: usize = 3;            // DoS protection
const CONNECTION_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const MAX_NEW_CONNECTIONS_PER_WINDOW: usize = 10;   // 10 new connections/minute
`

**Enforcement in can_accept_inbound() (lines 78-140):**
- Whitelisted masternodes bypass regular limits (but respect MAX_TOTAL_CONNECTIONS)
- Regular peers must have free slots in MAX_REGULAR_PEER_CONNECTIONS
- Per-IP limit of 3 connections (prevents IP spoofing)
- Rate limiting: max 10 new connections per minute

### 2.3 PeerConnectionRegistry (src/network/peer_connection_registry.rs line 50)

Tracks active connections with atomic counters:

`ust
pub struct PeerConnectionRegistry {
    inbound_count: AtomicUsize,      // Current inbound connection count
    outbound_count: AtomicUsize,     // Current outbound connection count
    connections: DashMap<String, ConnectionState>,  // Lock-free tracking
}
`

Methods:
- connected_count() = inbound_count + outbound_count
- outbound_count() returns current outbound count
- egister_peer_connection() increments counters
- mark_disconnected() decrements counters

---

## 3. MASTERNODE PEER EXCHANGE & DISCOVERY

### 3.1 GetMasternodes Request/Response Cycle

**Broadcast GetMasternodes:**  src/main.rs line 1168-1180

`ust
// Every 30 seconds: broadcast GetMasternodes to all connected peers
let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
loop {
    _ = interval.tick() => {
        tracing::debug!("📤 Broadcasting GetMasternodes to all peers");
        peer_connection_registry_clone
            .broadcast(NetworkMessage::GetMasternodes)
            .await;
    }
}
`

**Handler:** src/network/message_handler.rs line 858-897

`ust
async fn handle_get_masternodes(...) {
    let all_masternodes = context.masternode_registry.list_all().await;
    let mn_data: Vec<MasternodeAnnouncementData> = all_masternodes
        .iter()
        .map(|mn_info| {
            let ip_only = mn_info.masternode.address
                .split(':').next()
                .unwrap_or(&mn_info.masternode.address)
                .to_string();
            MasternodeAnnouncementData {
                address: ip_only,
                reward_address: mn_info.reward_address.clone(),
                tier: mn_info.masternode.tier,
                public_key: mn_info.masternode.public_key,
                collateral_outpoint: mn_info.masternode.collateral_outpoint.clone(),
                registered_at: mn_info.masternode.registered_at,
            }
        })
        .collect();
    Ok(Some(NetworkMessage::MasternodesResponse(mn_data)))
}
`

### 3.2 MasternodesResponse Handling (src/network/message_handler.rs line 2482-2571)

`ust
async fn handle_masternodes_response(
    &self,
    masternodes: Vec<MasternodeAnnouncementData>,
    context: &MessageContext,
) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let current_height = context.blockchain.get_height();
    let is_bootstrap = current_height == 0;  // Genesis block
    
    for mn_data in masternodes {
        // Skip self-overwrites
        if let Some(ref local_addr) = context.masternode_registry.get_local_address().await {
            let mn_ip = mn_data.address.split(':').next().unwrap_or(&mn_data.address);
            let local_ip = local_addr.split(':').next().unwrap_or(local_addr);
            if mn_ip == local_ip { continue; }
        }
        
        // Create Masternode struct (staked or free)
        let masternode = if let Some(outpoint) = mn_data.collateral_outpoint {
            Masternode::new_with_collateral(...)
        } else {
            Masternode::new_legacy(...)
        };
        
        // BOOTSTRAP MODE: Mark as ACTIVE at genesis (height 0)
        // NORMAL MODE: Mark as INACTIVE (will become active via direct P2P)
        let should_activate = is_bootstrap;
        
        context.masternode_registry
            .register_internal(masternode, mn_data.reward_address, should_activate)
            .await?;
        
        registered += 1;
    }
}
`

**KEY ISSUE:** Masternodes from MasternodesResponse are registered as INACTIVE in normal mode.
They are NOT automatically triggered to connect during Phase 1 startup because:
1. Phase 1 only happens ONCE at daemon startup
2. If a new masternode appears after Phase 1, it's in the registry but NOT in tier-sorted target list
3. Phase 3 explicitly skips masternodes (line 414-415): if masternode_registry.get(ip).is_some() { continue; }

### 3.3 MasternodeAnnouncement Broadcasting

**Source:** src/main.rs line 1276-1316

`ust
// Wait 10 seconds for initial peer connections
tokio::time::sleep(Duration::from_secs(10)).await;

// Build announcement using CURRENT registry state (for tier upgrades)
let build_announcement = |mn: &Masternode| NetworkMessage::MasternodeAnnouncementV3 {
    address: mn.address.clone(),
    reward_address: mn.wallet_address.clone(),
    tier: mn.tier,
    public_key: mn.public_key,
    collateral_outpoint: mn.collateral_outpoint.clone(),
    certificate: vec![0u8; 64],
    started_at: daemon_started_at,
};

// Broadcast ONCE immediately
let announcement = build_announcement(&mn_for_announcement);
peer_registry_for_announcement.broadcast(announcement).await;

// Continue broadcasting every 60 seconds
loop {
    tokio::time::sleep(Duration::from_secs(60)).await;
    let current_mn = registry_for_announcement.get(&mn_for_announcement.address).await;
    let announcement = build_announcement(&current_mn);
    peer_registry_for_announcement.broadcast(announcement).await;
}
`

**Handler:** src/network/message_handler.rs line 2226-2463

For staked tiers (Bronze/Silver/Gold):
1. Verifies collateral UTXO on-chain
2. Checks value matches tier requirement
3. Locks collateral (prevents double-spend)
4. Registers in registry
5. Relays to all other peers if NEW

---

## 4. ADAPTIVE RECONNECTION AI (src/ai/adaptive_reconnection.rs)

### 4.1 Connection History Tracking

`ust
pub struct PeerConnectionProfile {
    pub ip: String,
    pub is_masternode: bool,
    
    // Statistics
    pub total_connections: u64,
    pub successful_connections: u64,
    pub failed_connections: u64,
    pub consecutive_failures: u32,  // KEY: resets to 0 on success
    
    // Timing
    pub optimal_retry_delay_secs: f64,  // Learned from history
    pub reliability_score: f64,          // 0.0-1.0
}
`

### 4.2 Reconnection Logic

**Default Config:** src/ai/adaptive_reconnection.rs line 97-108

`ust
pub struct ReconnectionConfig {
    pub min_retry_delay_secs: f64,           // 2.0
    pub max_retry_delay_secs: f64,           // 300.0 (5 min)
    pub backoff_multiplier: f64,             // 1.5x exponential
    pub reliability_threshold: f64,          // 0.3 (30%)
    pub max_consecutive_failures: u32,       // 3
    pub cooldown_period_secs: u64,           // 600 (10 min)
    pub learning_rate: f64,                  // 0.1
}
`

**Advice Algorithm (client.rs line 436):**

`ust
let advice = reconnection_ai.get_reconnection_advice(ip, is_masternode);
if !advice.should_attempt {
    tracing::debug!("⏭️ Skipping {} (AI cooldown: {})", ip, advice.reasoning);
    continue;
}
`

The AI returns:
- should_attempt: bool - Whether to try this peer
- delay_secs: u64 - How long to wait before next attempt
- priority: ReconnectionPriority - Critical/High/Normal/Low/Skip
- easoning: String - Why this decision

**For masternodes:** Uses priority=Critical (reconnect immediately)
**For regular peers:** Uses learned delays based on success rate

---

## 5. NETWORK HEALTH MONITORING (src/main.rs line 1187-1269)

### 5.1 Health Check Task

Runs every 60 seconds (after 30-second startup delay):

`ust
// Line 1191-1269: Health monitoring task
let health = health_registry.check_network_health().await;

match health.status {
    HealthStatus::Critical => {  // < 25% active masternodes
        tracing::error!("🚨 CRITICAL: {} active / {} total masternodes",
                        health.active_masternodes, health.total_masternodes);
    }
    HealthStatus::Warning => {   // 25-50% active
        tracing::warn!("⚠️ WARNING: ...");
    }
    HealthStatus::Degraded => {  // 50-75% active
        tracing::info!("📊 Network degraded: ...");
    }
    HealthStatus::Healthy => {   // > 75% active
        tracing::debug!("✓ Network healthy: ...");
    }
}

// If unhealthy (< 5 active), attempt reconnection to inactive masternodes
if health.active_masternodes < 5 {
    let inactive_addresses = health_registry.get_inactive_masternode_addresses().await;
    for address in &inactive_addresses {
        let addr = address.clone();
        tokio::spawn(async move {
            if pm.add_peer(addr.clone()).await {
                tracing::info!("   ✓ Reconnection attempt to {}", addr);
            }
        });
    }
}
`

**Key Struct:** struct Masternode in registry

A masternode becomes ACTIVE only when:
1. Registered via MasternodeRegistry::register()
2. AND has a live P2P connection (peer_registry.is_connected(ip))

A masternode becomes INACTIVE when:
1. P2P connection drops
2. Or after timeout without heartbeat

---

## 6. BLOCKERS TO FULL MESH TOPOLOGY

### 6.1 **Blocker 1: Phase 1 Only Runs Once**

**Issue:** PHASE 1 pyramid startup only executes on daemon startup, not periodically.

`ust
// Line 175-341 in client.rs: PHASE 1 startup (one-time)
// Then PHASE 3 runs in loop (line 375-475)

// But PHASE 3 explicitly SKIPS masternodes:
if masternode_registry.get(ip).await.is_some() {
    continue;  // Skip masternodes in Phase 3!
}
`

**Impact:** 
- At startup, only tier-based subset targets are tried (~20 max for large networks)
- If new masternodes appear later (via MasternodesResponse), they are registered but NOT connected
- Phase 3 never retries masternodes because they're in the registry
- Result: **4-6 connections instead of possible 50**

### 6.2 **Blocker 2: reserved_masternode_slots = 20**

**Code:**
`ust
let reserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30);
// With max_peers=50: 50*40/100=20, clamped to [20,30] → 20 slots
`

**Issue:** 
- Even if pyramid code wanted to connect to all masternodes, only 20 slots reserved
- Large networks with 50+ masternodes can't fit all into 20 slots
- Remaining 30 slots go to "regular peers" (non-masternodes)

**For full mesh of N masternodes:** Need eserved_masternode_slots ≥ N
But: 20 ≤ reserved_masternode_slots ≤ 30 is HARD CLAMPED

### 6.3 **Blocker 3: AI Cooldown After Failures**

**Code (client.rs line 436-442):**
`ust
let advice = reconnection_ai.get_reconnection_advice(ip, false);
if !advice.should_attempt {
    tracing::debug!("⏭️ [PHASE3-PEER] Skipping {} (AI cooldown: {})", ip, ...);
    continue;
}
`

**Issue:**
- If a peer connection fails consistently (network partition, overload), AI puts it on cooldown
- Default: exponential backoff up to 5 minutes
- Peers are not retried for long periods even if they recover

### 6.4 **Blocker 4: Masternode Inactivity Window**

**Code (message_handler.rs line 2501-2545):**
`ust
let is_bootstrap = current_height == 0;
let should_activate = is_bootstrap;  // Only activate at genesis!

context.masternode_registry
    .register_internal(masternode, mn_data.reward_address, should_activate)
    .await?;
`

**Issue:**
- In normal mode (height > 0), masternodes from MasternodesResponse register as INACTIVE
- INACTIVE masternodes don't show in list_active() for Phase 1 targeting
- They only become ACTIVE when peer has a live connection (chicken-and-egg)
- Phase 3 never tries to connect because they're already in registry

### 6.5 **Blocker 5: UTXO State Divergence**

**Code (message_handler.rs line 2785-2883):**

If your UTXO state diverges from peers:
1. Peer sends UTXOStateHashResponse
2. You cache it and compare
3. If majority of peers have different hash → you request their full UTXO set
4. You pause normal peer operations while syncing

**Issue:** During UTXO reconciliation, no new peer connections are established until sync completes

---

## 7. ACTUAL BEHAVIOR (Why You See 4-6 Connections)

**Scenario: 15-node testnet**

**Node Startup (15 total masternodes):**
1. PHASE 1 (line 225-308): Total nodes = 15 ≤ 20 (FULL_MESH_THRESHOLD)
   - Should target all 15 masternodes
   - Actually targets: min(15, reserved_masternode_slots=20) = 15 ✓
   
2. But: Only launches connection tasks for reachable peers
   - Some peers may be offline/unresponsive
   - Some may reject inbound due to rate limits
   - Some may time out during TLS handshake
   
3. **Result after PHASE 1:** Connects to maybe 8-10 nodes (depends on peer availability)

4. PHASE 3 (every 30 seconds): 
   - Recalculates available_slots = 50 - (current_connected_count)
   - Skips all masternodes (already in registry)
   - Tries to fill remaining slots with regular peers
   - But no regular peers in discovery (testnet has only masternodes)
   - Result: **Stays at 8-10 connections**

5. **GetMasternodes broadcast (every 30 sec):**
   - Peers respond with their masternode lists
   - Receives same 15 masternodes you already know about
   - They're already registered → no new connections

6. **MasternodeAnnouncements (every 60 sec):**
   - Your node broadcasts its announcement to connected peers (8-10)
   - Other nodes hear about you
   - But only via gossip (not direct connection)
   - Phase 3 doesn't re-trigger because you're already known

**Final State:** 4-6 connections stable because:
- Initial PHASE 1 connects to ~10-12
- Some drop due to network issues
- AI cooldown prevents retries
- PHASE 3 skips masternodes
- RESULT: **4-6 stable connections (not all 15)**

---

## 8. SOLUTIONS FOR FULL MESH CONNECTIVITY

### Solution 1: Increase reserved_masternode_slots

**Change:**
`ust
// Current: clamp [20, 30]
let reserved_masternode_slots = (max_peers * 40 / 100).clamp(20, 30);

// To: Remove clamp or adjust percentage
let reserved_masternode_slots = (max_peers * 100 / 100);  // ALL slots for masternodes
// Or: clamp(50, 125)  // At least 50, max hardware limit
`

**Impact:** Allows up to 50 (or more) masternode connections instead of 20

### Solution 2: Retry Inactive Masternodes in PHASE 3

**Change:** src/network/client.rs line 414-415

`ust
// Current: Skip masternodes entirely
if masternode_registry.get(ip).await.is_some() { continue; }

// To: Include inactive masternodes
let info = masternode_registry.get(ip).await;
if let Some(mn_info) = info {
    if peer_registry.is_connected(&ip) {
        continue;  // Already connected
    }
    // Check if we should retry this masternode
    let advice = reconnection_ai.get_reconnection_advice(ip, true);  // is_masternode=true
    if !advice.should_attempt { continue; }
}
`

### Solution 3: Activate Masternodes on MasternodesResponse

**Change:** src/network/message_handler.rs line 2501-2545

`ust
// Current: Only activate at genesis (height 0)
let is_bootstrap = current_height == 0;
let should_activate = is_bootstrap;

// To: Always activate on discovery (triggers connection attempt)
let should_activate = true;  // Always mark as active
`

### Solution 4: Reduce AI Cooldown for Masternodes

**Change:** src/ai/adaptive_reconnection.rs 

`ust
impl ReconnectionConfig {
    pub fn for_masternode() -> Self {
        Self {
            min_retry_delay_secs: 5.0,      // (was 2.0)
            max_retry_delay_secs: 60.0,     // (was 300.0) - 1 min max for MN
            backoff_multiplier: 1.2,        // (was 1.5) - slower growth
            reliability_threshold: 0.2,     // (was 0.3) - more lenient
            max_consecutive_failures: 5,    // (was 3)   - more retries
            cooldown_period_secs: 120,      // (was 600) - 2 min vs 10 min
            learning_rate: 0.2,
        }
    }
}
`

### Solution 5: Periodic Full Mesh Check

**New Task:**
`ust
// Run every 5 minutes: ensure connected to all known masternodes
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        
        let all_masternodes = registry.list_all().await;
        let connected = peer_registry.get_connected_peers().await;
        
        for mn_info in all_masternodes {
            let ip = &mn_info.masternode.address;
            if !connected.contains(ip) {
                // Missing connection to known masternode
                if !connection_manager.is_active(ip) {
                    res.spawn(ip.clone(), true);  // Retry connection
                }
            }
        }
    }
});
`

---

## 9. KEY STRUCT DEFINITIONS & CONSTANTS

### Masternode Registry

`ust
pub struct MasternodeInfo {
    pub masternode: Masternode,
    pub reward_address: String,
    pub is_active: bool,  // Connected and ready
    pub registered_at: u64,
    pub last_seen: u64,
}

pub struct Masternode {
    pub address: String,  // IP only (no port)
    pub wallet_address: String,
    pub public_key: ed25519_dalek::VerifyingKey,
    pub tier: MasternodeTier,
    pub collateral: u64,
    pub collateral_outpoint: Option<OutPoint>,  // For staked tiers
    pub registered_at: u64,
}

pub enum MasternodeTier {
    Free,        // 0 collateral
    Bronze,      // 1000 TIME
    Silver,      // 10000 TIME
    Gold,        // 100000 TIME
}
`

### Connection State

`ust
pub struct ConnectionState {
    direction: ConnectionDirection,  // Inbound or Outbound
    connected_at: Instant,
}

pub enum ConnectionDirection {
    Inbound,   // Peer initiated
    Outbound,  // We initiated
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PeerConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}
`

### Network Configuration

`ust
pub struct NetworkConfig {
    pub listen_address: String,                    // :0 = any interface
    pub external_address: Option<String>,          // Public IP:port
    pub max_peers: u32,                            // User config (default 50)
    pub enable_upnp: bool,
    pub enable_peer_discovery: bool,               // Enable time-coin.io API
    pub bootstrap_peers: Vec<String>,              // Hardcoded fallback
    pub blacklisted_peers: Vec<String>,            // Permanent bans
    pub whitelisted_peers: Vec<String>,            // Trusted masternodes
}
`

---

## 10. SUMMARY TABLE

| Component | File | Key Constant | Limit | Notes |
|-----------|------|--------------|-------|-------|
| Max peers | config.rs | max_peers | 50 | User config |
| Masternode slots | client.rs:66 | reserved_masternode_slots | 20-30 | Clamped range |
| Total connections | connection_manager.rs:14 | MAX_TOTAL_CONNECTIONS | 125 | Hardware limit |
| Inbound only | connection_manager.rs:15 | MAX_INBOUND_CONNECTIONS | 100 | - |
| Outbound only | connection_manager.rs:16 | MAX_OUTBOUND_CONNECTIONS | 25 | - |
| Per-IP limit | connection_manager.rs:19 | MAX_CONNECTIONS_PER_IP | 3 | DoS protection |
| Rate limit | connection_manager.rs:20 | MAX_NEW_CONNECTIONS_PER_WINDOW | 10/min | - |
| Small network threshold | client.rs:196 | FULL_MESH_THRESHOLD | 20 nodes | Below = full mesh |
| Gold→Silver extras | client.rs:191 | GOLD_SILVER_EXTRAS | 3 | Extra visibility |
| Silver lateral | client.rs:192 | SILVER_LATERAL | 4 | Within tier |
| Bronze upward | client.rs:193 | BRONZE_UPWARD | 5 | To Silver |
| Bronze lateral | client.rs:194 | BRONZE_LATERAL | 3 | Within tier |
| Free upward | client.rs:195 | FREE_UPWARD | 5 | To Bronze |
| Phase 1 startup | client.rs:175 | ONE-TIME | - | Only on boot |
| Phase 2 fill | client.rs:342 | After Phase 1 | - | One round |
| Phase 3 discovery | client.rs:375 | LOOP EVERY 30s | - | Periodic |
| GetMasternodes broadcast | main.rs:1168 | EVERY 30s | - | Peer exchange |
| Masternode announcement | main.rs:1276 | EVERY 60s | - | After 10s startup delay |
| Health check | main.rs:1191 | EVERY 60s | - | After 30s delay |
| AI cooldown max | adaptive_reconnection.rs:101 | max_retry_delay_secs | 300s | 5 minute max |
| AI failures threshold | client.rs:425 | FORGET_THRESHOLD | 5 | Evict after 5 failures |

---

