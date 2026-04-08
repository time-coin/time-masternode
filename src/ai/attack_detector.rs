use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;

/// Dedup window: suppress re-reporting the same attack type from the same peer within this window.
const ATTACK_DEDUP_SECS: u64 = 300; // 5 minutes
/// Fork-bombing window: only flag if N forks occur within this sliding window.
const FORK_BOMB_WINDOW_SECS: u64 = 300; // 5 minutes
const FORK_BOMB_THRESHOLD: usize = 5;
/// DB key for persisted attacks.
const DB_KEY_ATTACKS: &[u8] = b"ai:attack_detector:attacks";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AttackType {
    EclipseAttack,       // Peer isolation attempt
    SybilAttack,         // Fake peer flooding
    TimingAttack,        // Clock manipulation
    DoublespendAttack,   // Multiple conflicting transactions
    ForkBombing,         // Intentional fork creation
    ResourceExhaustion,  // Memory/bandwidth exhaustion
    GossipEvictionStorm, // Repeated V4 eviction attempts for the same outpoint
    CollateralSpoofing,  // Attempting to claim another node's registered collateral
    SyncLoopFlooding,    // Excessive GetBlocks for same range (sync loop DoS)
    UtxoLockFlood,       // Peer sends excessive UTXOStateUpdate messages for one TX (DoS)
    SynchronizedCycling, // Coordinated synchronized disconnect/reconnect storm from a subnet
    TlsFlood,            // High-rate TLS handshake flood from distributed IPs
    PingFlood,           // Sustained ping-rate-limit excess from one peer — tokio RPC starvation
    MessageFlood,        // Raw pre-channel message flood (>500 msgs/s before deserialization)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttackPattern {
    pub attack_type: AttackType,
    pub confidence: f64, // 0.0 to 1.0
    pub severity: AttackSeverity,
    pub indicators: Vec<String>,
    pub first_detected: u64,
    pub last_seen: u64,
    pub source_ips: Vec<String>,
    pub recommended_action: MitigationAction,
    /// Set when the server enforcement loop has applied this pattern's mitigation action.
    /// Prevents the same detection from triggering repeated blacklist violations.
    #[serde(default)]
    pub mitigation_applied_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AttackSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MitigationAction {
    Monitor,
    RateLimitPeer(String),
    BlockPeer(String),
    AlertOperator,
    EmergencySync,
    HaltProduction,
    BanSubnet(String), // Ban an entire /24 (or custom CIDR) subnet
}

#[derive(Debug, Clone)]
struct PeerBehavior {
    _addr: String,
    connect_count: u32,
    disconnect_count: u32,
    invalid_messages: u32,
    pre_handshake_violations: u32,
    eviction_storm_attempts: u32,
    /// Timestamps of recent forks — entries older than FORK_BOMB_WINDOW_SECS are pruned.
    fork_timestamps: VecDeque<u64>,
    timestamp_drift: Vec<i64>,
    first_seen: u64,
    last_activity: u64,
}

#[derive(Debug, Clone)]
struct TransactionTracker {
    _txid: String,
    first_seen: u64,
    seen_count: u32,
    conflicting_versions: u32,
    source_peers: Vec<String>,
}

pub struct AttackDetector {
    db: Arc<Db>,
    peer_behaviors: Arc<RwLock<HashMap<String, PeerBehavior>>>,
    transaction_history: Arc<RwLock<HashMap<String, TransactionTracker>>>,
    detected_attacks: Arc<RwLock<Vec<AttackPattern>>>,
    time_window: Duration,
    /// Per-/24 subnet disconnect timestamps for SynchronizedCycling detection.
    /// Key: subnet prefix (e.g. "154.217.246"), Value: deque of disconnect Unix timestamps.
    subnet_disconnects: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-IP TLS failure timestamps for TlsFlood detection.
    /// Key: IP address, Value: deque of failure Unix timestamps.
    tls_failure_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-peer ping excess timestamps for PingFlood detection.
    ping_flood_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-peer raw message flood timestamps for MessageFlood detection.
    message_flood_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
}

impl AttackDetector {
    pub fn new(db: Arc<Db>) -> Result<Self, AppError> {
        let detected_attacks = Self::load_attacks_from_db(&db);
        Ok(Self {
            db,
            peer_behaviors: Arc::new(RwLock::new(HashMap::new())),
            transaction_history: Arc::new(RwLock::new(HashMap::new())),
            detected_attacks: Arc::new(RwLock::new(detected_attacks)),
            time_window: Duration::from_secs(300),
            subnet_disconnects: Arc::new(RwLock::new(HashMap::new())),
            tls_failure_times: Arc::new(RwLock::new(HashMap::new())),
            ping_flood_times: Arc::new(RwLock::new(HashMap::new())),
            message_flood_times: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    // ===== DB persistence =====

    fn load_attacks_from_db(db: &Db) -> Vec<AttackPattern> {
        match db.get(DB_KEY_ATTACKS) {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn persist_attacks(&self) {
        let attacks = self.detected_attacks.read();
        if let Ok(bytes) = serde_json::to_vec(&*attacks) {
            let _ = self.db.insert(DB_KEY_ATTACKS, bytes);
        }
    }

    // ===== Dedup helper =====

    /// Add an attack pattern, deduplicating against same-type + same-primary-source within the
    /// dedup window.  If a recent duplicate exists, `last_seen` is bumped and the list is
    /// re-persisted.  Returns `true` if a new entry was inserted.
    fn maybe_add_attack(&self, mut attack: AttackPattern) -> bool {
        let now = attack.first_detected;
        let primary_source = attack.source_ips.first().cloned().unwrap_or_default();

        let mut attacks = self.detected_attacks.write();

        for existing in attacks.iter_mut().rev() {
            if existing.attack_type == attack.attack_type
                && existing
                    .source_ips
                    .first()
                    .is_some_and(|s| *s == primary_source)
                && now.saturating_sub(existing.last_seen) <= ATTACK_DEDUP_SECS
            {
                // Duplicate within dedup window — just refresh the timestamp.
                existing.last_seen = now;
                drop(attacks);
                self.persist_attacks();
                return false;
            }
        }

        // Merge first_detected from any older entry for the same source/type so we preserve
        // the true onset time across restarts.
        if let Some(oldest) = attacks
            .iter()
            .filter(|a| {
                a.attack_type == attack.attack_type
                    && a.source_ips.first().is_some_and(|s| *s == primary_source)
            })
            .map(|a| a.first_detected)
            .min()
        {
            attack.first_detected = oldest;
        }

        attacks.push(attack);
        drop(attacks);
        self.persist_attacks();
        true
    }

    // ===== Public recording methods =====

    /// Record peer connection event
    pub fn record_peer_connect(&self, addr: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut behaviors = self.peer_behaviors.write();
        let behavior = behaviors.entry(addr.to_string()).or_insert(PeerBehavior {
            _addr: addr.to_string(),
            connect_count: 0,
            disconnect_count: 0,
            invalid_messages: 0,
            pre_handshake_violations: 0,
            eviction_storm_attempts: 0,
            fork_timestamps: VecDeque::new(),
            timestamp_drift: Vec::new(),
            first_seen: now,
            last_activity: now,
        });

        behavior.connect_count += 1;
        behavior.last_activity = now;

        // Check for rapid reconnection (Sybil attack indicator)
        if behavior.connect_count > 10 && (now - behavior.first_seen) < 60 {
            drop(behaviors);
            self.detect_sybil_attack(addr);
        }
    }

    /// Record peer disconnect
    pub fn record_peer_disconnect(&self, addr: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut behaviors = self.peer_behaviors.write();
        if let Some(behavior) = behaviors.get_mut(addr) {
            behavior.disconnect_count += 1;
            behavior.last_activity = now;
        }
    }

    /// Record invalid message from peer
    pub fn record_invalid_message(&self, addr: &str) {
        let mut behaviors = self.peer_behaviors.write();
        if let Some(behavior) = behaviors.get_mut(addr) {
            behavior.invalid_messages += 1;

            // High rate of invalid messages indicates malicious behavior
            if behavior.invalid_messages > 20 {
                drop(behaviors);
                self.flag_malicious_peer(addr);
            }
        }
    }

    /// Record fork from peer — uses a sliding time window to avoid false positives from
    /// legitimate but persistent forks.
    pub fn record_fork(&self, addr: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut behaviors = self.peer_behaviors.write();
        if let Some(behavior) = behaviors.get_mut(addr) {
            // Prune events outside the window before counting.
            while behavior
                .fork_timestamps
                .front()
                .is_some_and(|&t| now.saturating_sub(t) > FORK_BOMB_WINDOW_SECS)
            {
                behavior.fork_timestamps.pop_front();
            }
            behavior.fork_timestamps.push_back(now);
            behavior.last_activity = now;

            let recent_count = behavior.fork_timestamps.len();
            if recent_count >= FORK_BOMB_THRESHOLD {
                drop(behaviors);
                self.detect_fork_bombing(addr, recent_count);
            }
        }
    }

    /// Record timestamp from peer (for timing attack detection)
    pub fn record_timestamp(&self, addr: &str, drift_seconds: i64) {
        let mut behaviors = self.peer_behaviors.write();
        if let Some(behavior) = behaviors.get_mut(addr) {
            behavior.timestamp_drift.push(drift_seconds);

            // Keep only recent samples
            if behavior.timestamp_drift.len() > 10 {
                behavior.timestamp_drift.remove(0);
            }

            // Check for consistent clock manipulation
            if behavior.timestamp_drift.len() >= 5 {
                let avg_drift: i64 = behavior.timestamp_drift.iter().sum::<i64>()
                    / behavior.timestamp_drift.len() as i64;

                if avg_drift.abs() > 30 {
                    drop(behaviors);
                    self.detect_timing_attack(addr, avg_drift);
                }
            }
        }
    }

    /// Record transaction seen (for double-spend detection)
    pub fn record_transaction(&self, txid: &str, from_peer: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut history = self.transaction_history.write();
        let tracker = history
            .entry(txid.to_string())
            .or_insert(TransactionTracker {
                _txid: txid.to_string(),
                first_seen: now,
                seen_count: 0,
                conflicting_versions: 0,
                source_peers: Vec::new(),
            });

        tracker.seen_count += 1;
        if !tracker.source_peers.contains(&from_peer.to_string()) {
            tracker.source_peers.push(from_peer.to_string());
        }
    }

    /// Record conflicting transaction (double-spend attempt)
    pub fn record_conflicting_transaction(&self, txid: &str) {
        let mut history = self.transaction_history.write();
        if let Some(tracker) = history.get_mut(txid) {
            tracker.conflicting_versions += 1;

            if tracker.conflicting_versions >= 2 {
                let sources = tracker.source_peers.clone();
                drop(history);
                self.detect_doublespend_attempt(txid, sources);
            }
        }
    }

    /// Record that a V4 eviction attempt was blocked by the per-outpoint cooldown (storm in
    /// progress). After 3 blocked attempts from the same IP, classify as GossipEvictionStorm.
    pub fn record_eviction_storm_attempt(&self, addr: &str, outpoint: &str) {
        let now = Self::now_secs();
        let mut behaviors = self.peer_behaviors.write();
        let behavior = behaviors.entry(addr.to_string()).or_insert(PeerBehavior {
            _addr: addr.to_string(),
            connect_count: 0,
            disconnect_count: 0,
            invalid_messages: 0,
            pre_handshake_violations: 0,
            eviction_storm_attempts: 0,
            fork_timestamps: VecDeque::new(),
            timestamp_drift: Vec::new(),
            first_seen: now,
            last_activity: now,
        });
        behavior.eviction_storm_attempts += 1;
        behavior.last_activity = now;
        let attempts = behavior.eviction_storm_attempts;
        drop(behaviors);

        // Even a single blocked attempt is suspicious — detect immediately and escalate
        // confidence with repeated attempts.
        let confidence = (0.70 + (attempts.saturating_sub(1) as f64 * 0.05)).min(0.99);
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::GossipEvictionStorm,
            confidence,
            severity: AttackSeverity::Critical,
            indicators: vec![
                format!(
                    "{} eviction storm attempts for outpoint {} from {}",
                    attempts, outpoint, addr
                ),
                "V4 eviction blocked by per-outpoint cooldown".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    /// Record that a V4 announcement attempted to evict the local node (collateral spoofing).
    pub fn record_collateral_spoof_attempt(&self, addr: &str, outpoint: &str) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::CollateralSpoofing,
            confidence: 0.95,
            severity: AttackSeverity::Critical,
            indicators: vec![
                format!(
                    "V4 proof used to evict local node from outpoint {}",
                    outpoint
                ),
                format!("Attacker IP: {}", addr),
                "Gossip eviction of local node blocked — on-chain MasternodeReg required"
                    .to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    /// Record that a peer triggered the GetBlocks sync-loop detector.
    pub fn record_sync_flood(&self, addr: &str) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::SyncLoopFlooding,
            confidence: 0.80,
            severity: AttackSeverity::Medium,
            indicators: vec![
                format!(
                    "Peer {} sent ≥20 similar GetBlocks requests within 30s",
                    addr
                ),
                "Sync loop DoS pattern detected".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    /// Record that a peer sent too many UTXOStateUpdate messages for a single transaction.
    /// A legitimate TX with N inputs produces exactly N lock messages; flooding beyond a
    /// relay limit is a DoS pattern that can starve the tokio async runtime.
    pub fn record_utxo_lock_flood(&self, addr: &str, txid: &str, count: u32) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::UtxoLockFlood,
            confidence: 0.95,
            severity: AttackSeverity::High,
            indicators: vec![
                format!(
                    "Peer {} sent {} UTXOStateUpdate messages for TX {} (limit exceeded)",
                    addr, count, txid
                ),
                "UTXO lock flood DoS: starves async runtime and RPC handlers".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    /// Record that a peer sent a protocol message before completing the handshake.
    pub fn record_pre_handshake_violation(&self, addr: &str) {
        let now = Self::now_secs();
        let mut behaviors = self.peer_behaviors.write();
        let behavior = behaviors.entry(addr.to_string()).or_insert(PeerBehavior {
            _addr: addr.to_string(),
            connect_count: 0,
            disconnect_count: 0,
            invalid_messages: 0,
            pre_handshake_violations: 0,
            eviction_storm_attempts: 0,
            fork_timestamps: VecDeque::new(),
            timestamp_drift: Vec::new(),
            first_seen: now,
            last_activity: now,
        });
        behavior.pre_handshake_violations += 1;
        behavior.last_activity = now;
        let violations = behavior.pre_handshake_violations;
        drop(behaviors);

        // Only flag as attack after 3 pre-handshake violations (reduces false positives from
        // transient network issues or NAT traversal probes).
        // ≥10 violations → BlockPeer (persistent flooder like a port-scanner or probe bot).
        if violations >= 10 {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ResourceExhaustion,
                confidence: 0.95,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "{} pre-handshake violations from {} — persistent probe",
                        violations, addr
                    ),
                    "Peer repeatedly sends data before handshake; likely an automated attack"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else if violations >= 3 {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ResourceExhaustion,
                confidence: 0.75,
                severity: AttackSeverity::Medium,
                indicators: vec![
                    format!(
                        "{} pre-handshake message violations from {}",
                        violations, addr
                    ),
                    "Peer sends data before completing Version/Verack exchange".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record that a masternode at `addr` disconnected. If ≥5 nodes from the same /24
    /// subnet disconnect within 30 s, detect SynchronizedCycling and block the specific
    /// offending IP. The whole subnet is NOT banned automatically — operators can add
    /// explicit `bansubnet=` entries in time.conf if they are certain a subnet is hostile.
    pub fn record_synchronized_disconnect(&self, addr: &str) {
        // Extract /24 prefix (first 3 octets, e.g. "154.217.246")
        let subnet: String = addr.split('.').take(3).collect::<Vec<_>>().join(".");
        // Skip IPv6 addresses or anything that didn't parse as three octets
        if subnet.len() < 5 || subnet.contains(':') {
            return;
        }
        let now = Self::now_secs();
        const SYNC_WINDOW_SECS: u64 = 30;
        const SYNC_THRESHOLD: usize = 5;

        let should_ban = {
            let mut map = self.subnet_disconnects.write();
            let timestamps = map.entry(subnet.clone()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > SYNC_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len() >= SYNC_THRESHOLD
        };

        if should_ban {
            // Block the specific misbehaving IP, NOT the entire subnet.
            // Banning a whole /24 would collaterally affect legitimate nodes and operators
            // who share the same cloud provider (e.g. Alibaba, Hetzner).
            // Operators who are certain a subnet is hostile can still configure
            // `bansubnet=x.x.x.0/24` explicitly in time.conf.
            tracing::warn!(
                "🛡️ Synchronized disconnect storm detected from {}.x/24 (AV3) — blocking {}",
                subnet,
                addr
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::SynchronizedCycling,
                confidence: 0.85,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "≥{} nodes from {}.x disconnected within {}s",
                        SYNC_THRESHOLD, subnet, SYNC_WINDOW_SECS
                    ),
                    format!("Blocking specific offending IP {} (AV3)", addr),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a TLS handshake failure from `addr`. If ≥5 failures from the same IP
    /// occur within 60 s, detect TlsFlood and recommend BlockPeer.
    pub fn record_tls_failure(&self, addr: &str) {
        let now = Self::now_secs();
        const TLS_FLOOD_WINDOW_SECS: u64 = 60;
        const TLS_FLOOD_THRESHOLD: usize = 5;

        let should_block = {
            let mut map = self.tls_failure_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > TLS_FLOOD_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len() >= TLS_FLOOD_THRESHOLD
        };

        if should_block {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::TlsFlood,
                confidence: 0.88,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "≥{} TLS failures from {} within {}s",
                        TLS_FLOOD_THRESHOLD, addr, TLS_FLOOD_WINDOW_SECS
                    ),
                    "TLS handshake flood (AV13) — high-rate connection attempts before protocol"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a sustained ping rate-limit exceedance from `addr`.
    /// ≥3 excess events within 10 s → PingFlood → BlockPeer.
    pub fn record_ping_flood(&self, addr: &str) {
        let now = Self::now_secs();
        const PING_FLOOD_WINDOW_SECS: u64 = 10;
        const PING_FLOOD_THRESHOLD: usize = 3;

        let should_block = {
            let mut map = self.ping_flood_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > PING_FLOOD_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len() >= PING_FLOOD_THRESHOLD
        };

        if should_block {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::PingFlood,
                confidence: 0.90,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "≥{} ping rate-limit exceedances from {} within {}s",
                        PING_FLOOD_THRESHOLD, addr, PING_FLOOD_WINDOW_SECS
                    ),
                    "Sustained ping storm — starves tokio RPC thread, triggering watchdog false-restarts".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a raw pre-channel message flood event from `addr` (>500 msgs/s sustained).
    /// ≥2 flood events within 60 s → MessageFlood → BlockPeer.
    pub fn record_message_flood(&self, addr: &str) {
        let now = Self::now_secs();
        const MSG_FLOOD_WINDOW_SECS: u64 = 60;
        const MSG_FLOOD_THRESHOLD: usize = 2;

        let should_block = {
            let mut map = self.message_flood_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > MSG_FLOOD_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len() >= MSG_FLOOD_THRESHOLD
        };

        if should_block {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::MessageFlood,
                confidence: 0.95,
                severity: AttackSeverity::Critical,
                indicators: vec![
                    format!(
                        "≥{} raw message flood events from {} within {}s",
                        MSG_FLOOD_THRESHOLD, addr, MSG_FLOOD_WINDOW_SECS
                    ),
                    "Pre-channel message flood (>500 msgs/s) — bypasses rate limiters, saturates tokio workers".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Return attack patterns whose mitigation action has not yet been applied, and mark them
    /// as applied.  The enforcement loop calls this instead of `get_recent_attacks` so that each
    /// detected attack only triggers one blacklist violation — preventing rapid escalation to
    /// permanent ban from a single detection event.
    pub fn take_pending_mitigations(&self) -> Vec<AttackPattern> {
        let now = Self::now_secs();
        let mut attacks = self.detected_attacks.write();
        let mut pending = Vec::new();
        for attack in attacks.iter_mut() {
            if attack.mitigation_applied_at.is_none() {
                attack.mitigation_applied_at = Some(now);
                pending.push(attack.clone());
            }
        }
        drop(attacks);
        if !pending.is_empty() {
            self.persist_attacks();
        }
        pending
    }

    /// Check for eclipse attack (isolated from network)
    pub fn check_eclipse_attack(&self, connected_peer_count: usize, unique_ips: &[String]) -> bool {
        if connected_peer_count < 3 {
            return true;
        }

        // Check IP diversity
        let ip_prefixes: Vec<String> = unique_ips
            .iter()
            .filter_map(|ip| {
                ip.split('.')
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(".")
                    .split(':')
                    .next()
                    .map(|s| s.to_string())
            })
            .collect();

        let unique_prefixes: std::collections::HashSet<_> = ip_prefixes.iter().collect();
        let diversity_ratio = unique_prefixes.len() as f64 / ip_prefixes.len() as f64;

        if diversity_ratio < 0.5 {
            self.detect_eclipse_attack(unique_ips);
            return true;
        }

        false
    }

    // ===== Detection helpers =====

    fn detect_sybil_attack(&self, addr: &str) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::SybilAttack,
            confidence: 0.85,
            severity: AttackSeverity::High,
            indicators: vec![
                format!("Rapid reconnection from {}", addr),
                "Multiple connections in short timeframe".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    fn detect_fork_bombing(&self, addr: &str, recent_count: usize) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::ForkBombing,
            confidence: 0.9,
            severity: AttackSeverity::Critical,
            indicators: vec![
                format!(
                    "{} forks from {} within {}s window",
                    recent_count, addr, FORK_BOMB_WINDOW_SECS
                ),
                "Intentional chain disruption detected".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    fn detect_timing_attack(&self, addr: &str, avg_drift: i64) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::TimingAttack,
            confidence: 0.75,
            severity: AttackSeverity::Medium,
            indicators: vec![
                format!("Clock drift of {}s from {}", avg_drift, addr),
                "Potential timestamp manipulation".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    fn detect_doublespend_attempt(&self, txid: &str, sources: Vec<String>) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::DoublespendAttack,
            confidence: 0.95,
            severity: AttackSeverity::Critical,
            indicators: vec![
                format!("Conflicting versions of transaction {}", txid),
                format!("Sources: {}", sources.join(", ")),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: sources,
            recommended_action: MitigationAction::AlertOperator,
            mitigation_applied_at: None,
        });
    }

    fn detect_eclipse_attack(&self, peer_ips: &[String]) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::EclipseAttack,
            confidence: 0.8,
            severity: AttackSeverity::Critical,
            indicators: vec![
                "Low peer diversity detected".to_string(),
                "Potential network isolation".to_string(),
                format!(
                    "Connected to {} peers with limited IP diversity",
                    peer_ips.len()
                ),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: peer_ips.to_vec(),
            recommended_action: MitigationAction::EmergencySync,
            mitigation_applied_at: None,
        });
    }

    fn flag_malicious_peer(&self, addr: &str) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::ResourceExhaustion,
            confidence: 0.9,
            severity: AttackSeverity::High,
            indicators: vec![
                format!("High rate of invalid messages from {}", addr),
                "Malicious behavior detected".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    // ===== Query methods =====

    /// Get recent attacks
    pub fn get_recent_attacks(&self, since: Duration) -> Vec<AttackPattern> {
        let now = Self::now_secs();
        let cutoff = now.saturating_sub(since.as_secs());

        self.detected_attacks
            .read()
            .iter()
            .filter(|a| a.last_seen >= cutoff)
            .cloned()
            .collect()
    }

    /// Get all detected attacks
    pub fn get_all_attacks(&self) -> Vec<AttackPattern> {
        self.detected_attacks.read().clone()
    }

    /// Clear old attack records
    pub fn cleanup_old_records(&self, max_age: Duration) {
        let now = Self::now_secs();
        let cutoff = now.saturating_sub(max_age.as_secs());

        // Clean old peer behaviors
        let mut behaviors = self.peer_behaviors.write();
        behaviors.retain(|_, b| b.last_activity >= cutoff);

        // Clean old transaction history
        let mut history = self.transaction_history.write();
        history.retain(|_, t| t.first_seen >= cutoff);

        // Clean old attacks (keep for 24 hours)
        let attack_cutoff = now.saturating_sub(86_400);
        {
            let mut attacks = self.detected_attacks.write();
            attacks.retain(|a| a.last_seen >= attack_cutoff);
        }
        self.persist_attacks();
    }

    /// Get attack statistics
    pub fn get_statistics(&self) -> AttackStatistics {
        let attacks = self.detected_attacks.read();

        let mut stats = AttackStatistics {
            total_attacks: attacks.len(),
            by_type: HashMap::new(),
            by_severity: HashMap::new(),
            critical_count: 0,
        };

        for attack in attacks.iter() {
            *stats
                .by_type
                .entry(format!("{:?}", attack.attack_type))
                .or_insert(0) += 1;
            *stats
                .by_severity
                .entry(format!("{:?}", attack.severity))
                .or_insert(0) += 1;

            if attack.severity == AttackSeverity::Critical {
                stats.critical_count += 1;
            }
        }

        stats
    }

    // ===== Utilities =====

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Expose the configured time window (used by AISystem for cleanup calls).
    pub fn time_window(&self) -> Duration {
        self.time_window
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttackStatistics {
    pub total_attacks: usize,
    pub by_type: HashMap<String, usize>,
    pub by_severity: HashMap<String, usize>,
    pub critical_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sybil_detection() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        // Simulate rapid reconnections
        for _ in 0..15 {
            detector.record_peer_connect("192.168.1.1:8333");
        }

        let attacks = detector.get_all_attacks();
        assert!(!attacks.is_empty());
        assert_eq!(attacks[0].attack_type, AttackType::SybilAttack);
    }

    #[test]
    fn test_fork_bombing_detection() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        detector.record_peer_connect("192.168.1.1:8333");

        // Simulate multiple forks
        for _ in 0..6 {
            detector.record_fork("192.168.1.1:8333");
        }

        let attacks = detector.get_all_attacks();
        assert!(!attacks.is_empty());
        assert_eq!(attacks[0].attack_type, AttackType::ForkBombing);
    }

    #[test]
    fn test_fork_bombing_dedup() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        detector.record_peer_connect("192.168.1.1:8333");

        // First burst — should produce exactly one attack entry
        for _ in 0..10 {
            detector.record_fork("192.168.1.1:8333");
        }

        let attacks = detector.get_all_attacks();
        assert_eq!(
            attacks.len(),
            1,
            "dedup should collapse repeated detections"
        );
    }

    #[test]
    fn test_timing_attack_detection() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        detector.record_peer_connect("192.168.1.1:8333");

        // Simulate consistent clock drift
        for _ in 0..5 {
            detector.record_timestamp("192.168.1.1:8333", 45);
        }

        let attacks = detector.get_all_attacks();
        assert!(!attacks.is_empty());
        assert_eq!(attacks[0].attack_type, AttackType::TimingAttack);
    }

    #[test]
    fn test_eclipse_attack_detection() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        // All peers from same subnet
        let peers = vec![
            "192.168.1.1:8333".to_string(),
            "192.168.1.2:8333".to_string(),
        ];

        let is_attack = detector.check_eclipse_attack(2, &peers);
        assert!(is_attack);
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());

        {
            let detector = AttackDetector::new(db.clone()).unwrap();
            detector.record_peer_connect("10.0.0.1:8333");
            for _ in 0..6 {
                detector.record_fork("10.0.0.1:8333");
            }
            assert!(!detector.get_all_attacks().is_empty());
        }

        // Re-open with same DB — attacks should survive.
        let detector2 = AttackDetector::new(db).unwrap();
        assert!(
            !detector2.get_all_attacks().is_empty(),
            "attacks must persist across restarts"
        );
    }
}
