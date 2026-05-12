use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;

/// Convenience alias for per-subnet disconnect event queues.
/// Each entry is `(unix_timestamp_secs, ip_address_string)`.
type SubnetEventMap = Arc<RwLock<HashMap<String, VecDeque<(u64, String)>>>>;
type TimestampWindow = VecDeque<u64>;
type ConnectionFloodSources = HashMap<String, TimestampWindow>;

/// Dedup window: suppress re-reporting the same attack type from the same peer within this window.
const ATTACK_DEDUP_SECS: u64 = 300; // 5 minutes
/// Fork-bombing window: only flag if N forks occur within this sliding window.
const FORK_BOMB_WINDOW_SECS: u64 = 300; // 5 minutes
const FORK_BOMB_THRESHOLD: usize = 5;
/// DB key for persisted attacks.
const DB_KEY_ATTACKS: &[u8] = b"ai:attack_detector:attacks";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AttackType {
    EclipseAttack,            // Peer isolation attempt
    SybilAttack,              // Fake peer flooding
    TimingAttack,             // Clock manipulation
    DoublespendAttack,        // Multiple conflicting transactions
    ForkBombing,              // Intentional fork creation
    ResourceExhaustion,       // Memory/bandwidth exhaustion
    GossipEvictionStorm,      // Repeated V4 eviction attempts for the same outpoint
    CollateralSpoofing,       // Attempting to claim another node's registered collateral
    SyncLoopFlooding,         // Excessive GetBlocks for same range (sync loop DoS)
    UtxoLockFlood,            // Peer sends excessive UTXOStateUpdate messages for one TX (DoS)
    SynchronizedCycling,      // Coordinated synchronized disconnect/reconnect storm from a subnet
    TlsFlood,                 // High-rate TLS handshake flood from distributed IPs
    PingFlood,    // Sustained ping-rate-limit excess from one peer — tokio RPC starvation
    MessageFlood, // Raw pre-channel message flood (>500 msgs/s before deserialization)
    InvalidVoteSignatureSpam, // Forged Ed25519 vote signatures at ≥5/30s (AV27)
    UnregisteredVoterSpam, // Votes from unregistered IDs at ≥10/60s (AV28)
    FinalityInjectionSpam, // TransactionFinalized for unknown TXs to force 49-validator broadcast amplification (AV38)
    NullTransactionFlood, // Transactions with 0 inputs + 0 outputs to exhaust mempool at zero cost (AV39)
    ZeroCollateralPollution, // Register zero-value UTXOs as Free-tier collateral under victim IPs to poison registry (AV40)
    ConnectionFlood, // High-rate inbound connections rejected by rate limiter — subnet DoS (AV50)
    FrameBomb, // Crafted TCP frame header claiming multi-GB payload to OOM/crash the node (AV51)
    CollateralHijack, // MasternodeRegistration claiming a UTXO not owned by the registrant (AV52)
    RewardRedirect, // Claimed collateral UTXO owner address mismatches announced reward address — block rewards siphoning attempt (AV54)
    InvalidTxSpam, // Peer submits repeated invalid transactions (bad UTXO ref, bad signature, etc.) to waste validation CPU
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
    /// Prevents the same detection from triggering repeated banlist violations.
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
    /// Per-/24 subnet disconnect events for SynchronizedCycling detection.
    /// Key: subnet prefix (e.g. "154.217.246"), Value: deque of (Unix timestamp, IP address).
    /// The threshold is based on UNIQUE IPs, not raw event count, so a single peer
    /// reconnecting multiple times after an error does not trigger a false positive.
    subnet_disconnects: SubnetEventMap,
    /// Per-/16 subnet disconnect events for cross-/24 SynchronizedCycling detection.
    /// Catches attackers that spread nodes across multiple /24s within the same /16
    /// to stay under the per-/24 threshold (e.g. 47.79.38.x + 47.79.39.x + 47.79.32.x).
    /// Key: /16 prefix (e.g. "47.79"), Value: deque of (Unix timestamp, IP address).
    subnet16_disconnects: SubnetEventMap,
    /// Per-IP TLS failure timestamps for TlsFlood detection.
    /// Key: IP address, Value: deque of failure Unix timestamps.
    tls_failure_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-/24-subnet TLS failure timestamps for distributed TLS flood detection.
    /// An attacker spreading failures across many IPs in the same /24 — each staying
    /// under the per-IP threshold — is caught here instead.
    subnet_tls_failures: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-peer ping excess timestamps for PingFlood detection.
    ping_flood_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-peer raw message flood timestamps for MessageFlood detection.
    message_flood_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-peer TransactionFinalized injection timestamps for FinalityInjectionSpam detection (AV38).
    /// Key: peer IP, Value: deque of Unix timestamps for injected-finality events.
    finality_injection_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-peer null-TX flood timestamps for NullTransactionFlood detection (AV39).
    /// Key: peer IP, Value: deque of Unix timestamps for null-TX broadcast events.
    null_tx_flood_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-/24-subnet inbound connection rejection timestamps for ConnectionFlood detection (AV50).
    /// Key: /24 prefix (e.g. "47.82.254"), Value: per-IP deques of rejection Unix timestamps.
    connection_flood_times: Arc<RwLock<HashMap<String, ConnectionFloodSources>>>,
    /// Per-IP frame bomb timestamps for FrameBomb detection (AV51).
    /// Key: IP address, Value: deque of oversized-frame Unix timestamps.
    frame_bomb_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-IP collateral hijack attempt timestamps for CollateralHijack detection (AV52).
    /// Key: IP address, Value: deque of key-mismatch Unix timestamps.
    collateral_hijack_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-IP reward-redirect attempt tracking for RewardRedirect detection (AV54).
    /// Each entry records (unix_timestamp, outpoint_string) per IP so we can detect
    /// the same IP cycling through multiple outpoints with mismatched reward addresses.
    /// Key: IP address, Value: deque of (Unix timestamp, outpoint).
    reward_redirect_times: SubnetEventMap,
    /// Per-/24-subnet reward-redirect timestamps for detecting coordinated campaigns.
    /// Key: /24 prefix (e.g. "154.217.246"), Value: deque of (Unix timestamp, IP address).
    reward_redirect_subnet: SubnetEventMap,
    /// Per-IP zero-value collateral registration timestamps for ZeroCollateralPollution (AV40).
    /// Key: IP address, Value: deque of zero-collateral registration Unix timestamps.
    zero_collateral_pollution_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Per-IP invalid-transaction timestamps for InvalidTxSpam detection.
    /// Key: IP address, Value: deque of Unix timestamps when an invalid TX was received.
    invalid_tx_spam_times: Arc<RwLock<HashMap<String, VecDeque<u64>>>>,
    /// Fires when a new (non-duplicate) attack is detected so the enforcement
    /// loop can wake up immediately instead of waiting the full 30-second tick.
    ban_notify: Arc<tokio::sync::Notify>,
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
            subnet16_disconnects: Arc::new(RwLock::new(HashMap::new())),
            tls_failure_times: Arc::new(RwLock::new(HashMap::new())),
            subnet_tls_failures: Arc::new(RwLock::new(HashMap::new())),
            ping_flood_times: Arc::new(RwLock::new(HashMap::new())),
            message_flood_times: Arc::new(RwLock::new(HashMap::new())),
            finality_injection_times: Arc::new(RwLock::new(HashMap::new())),
            null_tx_flood_times: Arc::new(RwLock::new(HashMap::new())),
            connection_flood_times: Arc::new(RwLock::new(HashMap::new())),
            frame_bomb_times: Arc::new(RwLock::new(HashMap::new())),
            collateral_hijack_times: Arc::new(RwLock::new(HashMap::new())),
            reward_redirect_times: Arc::new(RwLock::new(HashMap::new())),
            reward_redirect_subnet: Arc::new(RwLock::new(HashMap::new())),
            zero_collateral_pollution_times: Arc::new(RwLock::new(HashMap::new())),
            invalid_tx_spam_times: Arc::new(RwLock::new(HashMap::new())),
            ban_notify: Arc::new(tokio::sync::Notify::new()),
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

    /// Returns the notifier that fires whenever a new (non-duplicate) attack is
    /// detected.  The AI enforcement loop in `server.rs` waits on this so it can
    /// apply mitigations immediately instead of waiting the full 30-second tick.
    pub fn ban_notifier(&self) -> Arc<tokio::sync::Notify> {
        self.ban_notify.clone()
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
                // Check if this is a severity escalation (e.g. RateLimitPeer → BlockPeer).
                // If so, upgrade the existing entry and wake the enforcement loop so the
                // stronger mitigation is applied immediately.
                let is_escalation = matches!(
                    (&existing.recommended_action, &attack.recommended_action),
                    (
                        MitigationAction::RateLimitPeer(_),
                        MitigationAction::BlockPeer(_)
                    ) | (
                        MitigationAction::Monitor,
                        MitigationAction::RateLimitPeer(_)
                    ) | (MitigationAction::Monitor, MitigationAction::BlockPeer(_))
                );
                existing.last_seen = now;
                if is_escalation {
                    tracing::warn!(
                        "🔺 AI: escalating mitigation for {:?} from {:?} → {:?}",
                        existing.attack_type,
                        existing.recommended_action,
                        attack.recommended_action
                    );
                    existing.recommended_action = attack.recommended_action;
                    existing.confidence = attack.confidence;
                    existing.severity = attack.severity;
                    existing.indicators.extend(attack.indicators);
                    existing.mitigation_applied_at = None; // re-arm so enforcement loop acts
                    drop(attacks);
                    self.ban_notify.notify_one();
                    self.persist_attacks();
                    return true;
                }
                // Re-arm an already-actioned BlockPeer attack after a cooldown so persistent
                // attackers accumulate additional violations and eventually reach the ban
                // threshold. Without this, the dedup window prevents re-enforcement and a
                // sustained flooder never gets more than 2 violations (< 3 needed for a ban).
                const REARM_COOLDOWN_SECS: u64 = 60;
                if matches!(&attack.recommended_action, MitigationAction::BlockPeer(_)) {
                    if let Some(applied_at) = existing.mitigation_applied_at {
                        if now.saturating_sub(applied_at) >= REARM_COOLDOWN_SECS {
                            existing.mitigation_applied_at = None;
                            drop(attacks);
                            self.ban_notify.notify_one();
                            self.persist_attacks();
                            return true;
                        }
                    }
                }
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
        // Wake the enforcement loop immediately so it can apply mitigations without
        // waiting the full 30-second periodic tick.
        self.ban_notify.notify_one();
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

    /// Record a reward-redirect attempt (AV54): the node claimed a collateral outpoint whose
    /// on-chain UTXO is owned by a different address than the announced wallet/reward address.
    ///
    /// Single occurrence → `BlockPeer` immediately (the attack is unambiguous and deliberate).
    /// 3+ distinct IPs from the same /24 within 3 600 s → `BanSubnet` (coordinated campaign
    /// targeting reward payout for multiple masternodes simultaneously).
    pub fn record_reward_redirect_attempt(&self, addr: &str, outpoint: &str) {
        let now = Self::now_secs();
        let subnet = addr.rsplitn(2, '.').last().unwrap_or(addr).to_string();

        // --- Per-IP tracking ---
        let distinct_outpoints = {
            let mut ip_map = self.reward_redirect_times.write();
            let window = ip_map.entry(addr.to_string()).or_default();
            // Prune entries older than 3 600 s
            while window
                .front()
                .map(|(t, _)| now.saturating_sub(*t) > 3_600)
                .unwrap_or(false)
            {
                window.pop_front();
            }
            window.push_back((now, outpoint.to_string()));
            // Count distinct outpoints attempted in the window
            let mut seen = std::collections::HashSet::new();
            for (_, op) in window.iter() {
                seen.insert(op.as_str());
            }
            seen.len()
        };

        // --- Per-/24 subnet tracking ---
        let subnet_ips = {
            let mut sub_map = self.reward_redirect_subnet.write();
            let window = sub_map.entry(subnet.clone()).or_default();
            while window
                .front()
                .map(|(t, _)| now.saturating_sub(*t) > 3_600)
                .unwrap_or(false)
            {
                window.pop_front();
            }
            window.push_back((now, addr.to_string()));
            let mut seen_ips = std::collections::HashSet::new();
            for (_, ip) in window.iter() {
                seen_ips.insert(ip.as_str());
            }
            seen_ips.len()
        };

        let (action, confidence, severity, campaign_note) = if subnet_ips >= 3 {
            // 3+ distinct IPs from the same /24 → coordinated reward-siphoning campaign
            (
                MitigationAction::BanSubnet(format!("{}.0/24", subnet)),
                1.0_f64,
                AttackSeverity::Critical,
                format!(
                    "Coordinated reward-redirect campaign: {} distinct IPs from /{}.0/24 \
                     attempted to siphon block rewards within 1 h",
                    subnet_ips, subnet
                ),
            )
        } else {
            (
                MitigationAction::BlockPeer(addr.to_string()),
                0.99_f64,
                AttackSeverity::High,
                format!(
                    "Single-IP reward-redirect: {} distinct outpoint(s) attempted with \
                     mismatched reward address in 1 h window",
                    distinct_outpoints
                ),
            )
        };

        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::RewardRedirect,
            confidence,
            severity,
            indicators: vec![
                format!(
                    "Peer {} claimed collateral {} but UTXO owner ≠ announced reward address",
                    addr, outpoint
                ),
                campaign_note,
                "Old collateral UTXO already gone — attack is unambiguous (not churn)".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: action,
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

    /// Record that a masternode at `addr` disconnected. If ≥5 DISTINCT IPs from the same
    /// /24 subnet disconnect within 30 s, detect SynchronizedCycling and block the specific
    /// offending IP. The whole subnet is NOT banned automatically — operators can add
    /// explicit `bansubnet=` entries in time.conf if they are certain a subnet is hostile.
    ///
    /// Counting is based on UNIQUE IPs, not raw disconnect events. This prevents false
    /// positives when a single legitimate peer reconnects multiple times after a frame error
    /// or TLS race — that looks like 5 disconnects from one IP, not 5 distinct peers. Only
    /// a genuine coordinated storm (multiple different hosts dropping simultaneously) triggers
    /// the threshold.
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
            let events = map.entry(subnet.clone()).or_default();
            // Expire events outside the window.
            while events
                .front()
                .map(|(t, _)| now.saturating_sub(*t) > SYNC_WINDOW_SECS)
                .unwrap_or(false)
            {
                events.pop_front();
            }
            events.push_back((now, addr.to_string()));
            // Count UNIQUE IPs in the window — a single peer reconnecting N times
            // must not be mistaken for N distinct attackers.
            let unique_ips: std::collections::HashSet<&str> =
                events.iter().map(|(_, ip)| ip.as_str()).collect();
            unique_ips.len() >= SYNC_THRESHOLD
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

        // /16 cross-subnet check: catch attackers that spread nodes across multiple /24s
        // within the same /16 to stay under the per-/24 threshold.
        // Example: 47.79.38.x + 47.79.39.x + 47.79.32.x all part of the same attack cluster.
        // When ≥5 distinct IPs from the same /16 disconnect within 30s, block each one.
        // We do NOT auto-ban the /16 — that would be 65,536 addresses and would catch
        // legitimate nodes on the same cloud AS.
        let sixteen_prefix: String = addr.split('.').take(2).collect::<Vec<_>>().join(".");
        if sixteen_prefix.len() >= 3 && !sixteen_prefix.contains(':') {
            const SYNC16_WINDOW_SECS: u64 = 30;
            // Raised from 5 → 15: large cloud providers (Alibaba, AWS, Hetzner) legitimately
            // host many masternodes on the same /16. The original threshold of 5 fired during
            // normal partition-recovery reconnect storms and cascaded into self-inflicted bans.
            const SYNC16_THRESHOLD: usize = 15;

            let ips_to_block: Vec<String> = {
                let mut map = self.subnet16_disconnects.write();
                let events = map.entry(sixteen_prefix.clone()).or_default();
                while events
                    .front()
                    .map(|(t, _)| now.saturating_sub(*t) > SYNC16_WINDOW_SECS)
                    .unwrap_or(false)
                {
                    events.pop_front();
                }
                events.push_back((now, addr.to_string()));
                let unique_ips: std::collections::HashSet<String> =
                    events.iter().map(|(_, ip)| ip.clone()).collect();
                if unique_ips.len() >= SYNC16_THRESHOLD {
                    // Drain the window after firing so subsequent disconnects from the same
                    // /16 must accumulate a fresh batch before triggering again. Without this,
                    // every new disconnect after threshold kept re-blocking all prior IPs.
                    events.clear();
                    unique_ips.into_iter().collect()
                } else {
                    vec![]
                }
            };

            if !ips_to_block.is_empty() {
                tracing::warn!(
                    "🛡️ Cross-/24 synchronized disconnect from {}.x.x/16 (AV3) — blocking {} IPs",
                    sixteen_prefix,
                    ips_to_block.len()
                );
                for ip in ips_to_block {
                    self.maybe_add_attack(AttackPattern {
                        attack_type: AttackType::SynchronizedCycling,
                        confidence: 0.90,
                        severity: AttackSeverity::High,
                        indicators: vec![
                            format!(
                                "≥{} nodes from {}.x.x/16 disconnected within {}s (cross-/24 AV3)",
                                SYNC16_THRESHOLD, sixteen_prefix, SYNC16_WINDOW_SECS
                            ),
                            format!("Blocking individual IP {} from /16 cluster", ip),
                        ],
                        first_detected: now,
                        last_seen: now,
                        source_ips: vec![ip.clone()],
                        recommended_action: MitigationAction::BlockPeer(ip),
                        mitigation_applied_at: None,
                    });
                }
            }
        }
    }

    /// Two thresholds are checked:
    /// 1. Per-IP: ≥5 failures from the same IP within 60s → BlockPeer (per-IP TLS flood).
    /// 2. Per-/24 subnet: ≥20 failures from the same /24 within 60s → BlockPeer for the
    ///    specific triggering IP. This catches distributed attacks spread across many IPs
    ///    that each stay under the per-IP threshold. Subnet-wide bans are NOT issued —
    ///    honest nodes on shared cloud infrastructure would be caught in the blast radius.
    pub fn record_tls_failure(&self, addr: &str) {
        let now = Self::now_secs();
        const TLS_FLOOD_WINDOW_SECS: u64 = 60;
        const TLS_FLOOD_THRESHOLD: usize = 5;
        const TLS_SUBNET_FLOOD_THRESHOLD: usize = 20;

        // Per-IP check
        let ip_should_block = {
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

        if ip_should_block {
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

        // Per-/24-subnet check — catches distributed TLS floods below the per-IP threshold
        let subnet: String = addr.split('.').take(3).collect::<Vec<_>>().join(".");
        if subnet.len() >= 5 && !subnet.contains(':') {
            let subnet_should_block = {
                let mut map = self.subnet_tls_failures.write();
                let timestamps = map.entry(subnet.clone()).or_default();
                while timestamps
                    .front()
                    .map(|t| now.saturating_sub(*t) > TLS_FLOOD_WINDOW_SECS)
                    .unwrap_or(false)
                {
                    timestamps.pop_front();
                }
                timestamps.push_back(now);
                timestamps.len() >= TLS_SUBNET_FLOOD_THRESHOLD
            };

            if subnet_should_block && !ip_should_block {
                tracing::warn!(
                    "🛡️ Distributed TLS flood from {}.x/24 (AV13 subnet variant) — blocking {}",
                    subnet,
                    addr
                );
                self.maybe_add_attack(AttackPattern {
                    attack_type: AttackType::TlsFlood,
                    confidence: 0.80,
                    severity: AttackSeverity::High,
                    indicators: vec![
                        format!(
                            "≥{} TLS failures from {}.x/24 within {}s (distributed, each IP below per-IP threshold)",
                            TLS_SUBNET_FLOOD_THRESHOLD, subnet, TLS_FLOOD_WINDOW_SECS
                        ),
                        format!("Blocking specific offending IP {} (not whole subnet)", addr),
                    ],
                    first_detected: now,
                    last_seen: now,
                    source_ips: vec![addr.to_string()],
                    recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                    mitigation_applied_at: None,
                });
            }
        }
    }

    /// Record that a peer sent forged/invalid Ed25519 vote signatures after the
    /// per-peer sliding window threshold was exceeded (AV27: ≥5 in 30s).
    /// This makes the attack visible to the AI layer for cross-peer correlation.
    pub fn record_invalid_vote_sig_spam(&self, addr: &str) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::InvalidVoteSignatureSpam,
            confidence: 0.90,
            severity: AttackSeverity::Medium,
            indicators: vec![
                format!(
                    "≥5 invalid Ed25519 vote signatures from {} within 30s (AV27)",
                    addr
                ),
                "Possible forged vote spam to burn CPU on signature verification".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    /// Record that a peer sent votes from unregistered IDs after the per-peer
    /// sliding window threshold was exceeded (AV28: ≥10 in 60s).
    /// This makes the attack visible to the AI layer for cross-peer correlation.
    pub fn record_unregistered_voter_spam(&self, addr: &str) {
        let now = Self::now_secs();
        self.maybe_add_attack(AttackPattern {
            attack_type: AttackType::UnregisteredVoterSpam,
            confidence: 0.85,
            severity: AttackSeverity::Low,
            indicators: vec![
                format!(
                    "≥10 votes from unregistered IDs relayed by {} within 60s (AV28)",
                    addr
                ),
                "Possible spam via relay of votes for deregistered/phantom masternodes".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
            mitigation_applied_at: None,
        });
    }

    /// Record that a peer sent `TransactionFinalized` for a TX unknown to this node,
    /// or forwarded a null TX via `TransactionFinalized` (AV38+AV39 combined attack).
    ///
    /// A single occurrence may be an honest relay node that received the attacker's flood
    /// and forwarded it before our structural check could stop it.  We therefore use a
    /// relay-safe mitigation: the first threshold (≥5/30s) triggers `RateLimitPeer` only;
    /// a secondary threshold (≥20/30s) — reachable only by the true originator or a
    /// heavily-compromised relay — escalates to `BlockPeer`.
    pub fn record_finality_injection(&self, addr: &str) {
        let now = Self::now_secs();
        const FINALITY_INJECT_WINDOW_SECS: u64 = 30;
        const RATE_LIMIT_THRESHOLD: usize = 5;
        const BLOCK_THRESHOLD: usize = 20;

        let count = {
            let mut map = self.finality_injection_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > FINALITY_INJECT_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len()
        };

        if count >= BLOCK_THRESHOLD {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::FinalityInjectionSpam,
                confidence: 0.97,
                severity: AttackSeverity::Critical,
                indicators: vec![
                    format!(
                        "≥{} TransactionFinalized injections for unknown/null TXs from {} within {}s (AV38+AV39) — likely originator",
                        BLOCK_THRESHOLD, addr, FINALITY_INJECT_WINDOW_SECS
                    ),
                    "Volume exceeds honest relay capacity — source is generating novel TXIDs".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else if count >= RATE_LIMIT_THRESHOLD {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::FinalityInjectionSpam,
                confidence: 0.75,
                severity: AttackSeverity::Medium,
                indicators: vec![
                    format!(
                        "≥{} TransactionFinalized injections for unknown/null TXs from {} within {}s (AV38) — may be relay",
                        RATE_LIMIT_THRESHOLD, addr, FINALITY_INJECT_WINDOW_SECS
                    ),
                    "Rate-limiting rather than banning to protect innocent relay nodes".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a null-transaction broadcast from `addr` (0 inputs, 0 outputs, no special_data).
    ///
    /// A single occurrence is not penalised — the peer may be an innocent relay that forwarded
    /// the TX before our validation could stop it.  Only if the same peer sends ≥3 distinct
    /// null TXs within 60 s do we conclude it is the originator (or an aggressive relay that
    /// deserves the same treatment) and escalate to BlockPeer.
    ///
    /// Honest relay nodes only ever forward each unique TX once (bloom-filter dedup prevents
    /// re-relay), so they will never accumulate 3 events within the window.
    pub fn record_null_tx_flood(&self, addr: &str) {
        let now = Self::now_secs();
        const NULL_TX_WINDOW_SECS: u64 = 60;
        const NULL_TX_THRESHOLD: usize = 3;

        let should_block = {
            let mut map = self.null_tx_flood_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > NULL_TX_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len() >= NULL_TX_THRESHOLD
        };

        if should_block {
            tracing::warn!(
                "🚫 NullTransactionFlood detected from {} — ≥{} null TXs within {}s (AV39)",
                addr,
                NULL_TX_THRESHOLD,
                NULL_TX_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::NullTransactionFlood,
                confidence: 0.95,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "≥{} null transactions (0 inputs, 0 outputs) from {} within {}s (AV39)",
                        NULL_TX_THRESHOLD, addr, NULL_TX_WINDOW_SECS
                    ),
                    "Null TXs cost nothing to produce and never clear from the mempool — \
                     relay nodes only forward each TX once so repeated sends indicate the originator"
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

    /// Record that an inbound connection from `addr` was rejected by the rate limiter (AV50).
    ///
    /// Rate-limit rejections are cheap to generate (attacker just opens TCP sockets) and the
    /// existing `can_accept_inbound` guard stops connections at the OS level — but the AI has no
    /// visibility unless we record here.  We track per-/24 subnet to catch distributed floods
    /// where each individual IP stays under per-IP thresholds.
    ///
    /// Thresholds:
    /// - Single noisy IP: ≥4 rejections/60s → `RateLimitPeer`, ≥8 rejections/60s → `BlockPeer`
    /// - Coordinated subnet flood: ≥10 total rejections/60s from ≥3 unique IPs in the same /24
    ///   → `BanSubnet`
    ///
    /// This avoids poisoning an entire subnet when only one or two misconfigured or stale nodes
    /// are retrying too aggressively.
    pub fn record_connection_flood(&self, addr: &str) {
        let now = Self::now_secs();
        const CONN_FLOOD_WINDOW_SECS: u64 = 60;
        const CONN_FLOOD_RATE_THRESHOLD: usize = 4;
        const CONN_FLOOD_BLOCK_THRESHOLD: usize = 8;
        const CONN_FLOOD_SUBNET_THRESHOLD: usize = 10;
        const CONN_FLOOD_SUBNET_UNIQUE_IPS: usize = 3;

        let subnet: String = addr.split('.').take(3).collect::<Vec<_>>().join(".");
        if subnet.len() < 5 || subnet.contains(':') {
            return;
        }

        let (ip_rejections, total_rejections, unique_ips) = {
            let mut map = self.connection_flood_times.write();
            let subnet_sources = map.entry(subnet.clone()).or_default();
            let timestamps = subnet_sources.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > CONN_FLOOD_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);

            subnet_sources.retain(|_, entries| {
                while entries
                    .front()
                    .map(|t| now.saturating_sub(*t) > CONN_FLOOD_WINDOW_SECS)
                    .unwrap_or(false)
                {
                    entries.pop_front();
                }
                !entries.is_empty()
            });

            let total = subnet_sources.values().map(VecDeque::len).sum::<usize>();
            let unique = subnet_sources.len();
            let ip_total = subnet_sources.get(addr).map(VecDeque::len).unwrap_or(0);
            (ip_total, total, unique)
        };

        if ip_rejections >= CONN_FLOOD_BLOCK_THRESHOLD {
            tracing::warn!(
                "🛡️ Inbound connection flood from {} (AV50) — {} rate-limited rejections in {}s — blocking peer",
                addr,
                ip_rejections,
                CONN_FLOOD_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ConnectionFlood,
                confidence: 0.88,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "{} inbound connections rejected by rate limiter within {}s",
                        ip_rejections, CONN_FLOOD_WINDOW_SECS
                    ),
                    "Single-IP connection flood — block offending peer without penalizing its subnet"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else if ip_rejections >= CONN_FLOOD_RATE_THRESHOLD {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ConnectionFlood,
                confidence: 0.78,
                severity: AttackSeverity::Medium,
                indicators: vec![
                    format!(
                        "{} inbound connections rejected by rate limiter within {}s",
                        ip_rejections, CONN_FLOOD_WINDOW_SECS
                    ),
                    "Single-IP reconnect churn — rate-limit peer before escalating to subnet action"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }

        if total_rejections >= CONN_FLOOD_SUBNET_THRESHOLD
            && unique_ips >= CONN_FLOOD_SUBNET_UNIQUE_IPS
        {
            tracing::warn!(
                "🛡️ Inbound connection flood from {}.x/24 (AV50) — {} rate-limited rejections across {} IPs in {}s — banning subnet",
                subnet,
                total_rejections,
                unique_ips,
                CONN_FLOOD_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ConnectionFlood,
                confidence: 0.92,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "{} inbound connections from {}.x/24 rejected by rate limiter within {}s ({} unique IPs, AV50)",
                        total_rejections, subnet, CONN_FLOOD_WINDOW_SECS, unique_ips
                    ),
                    "Coordinated connection flood — subnet rate-limited; distributed botnet pattern".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![format!("{}.0/24", subnet)],
                recommended_action: MitigationAction::BanSubnet(format!("{}.0/24", subnet)),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a crafted oversized-frame attack from `addr` (AV51).
    ///
    /// Sending a 4-byte frame-length header claiming a multi-GB body requires only 4 bytes of
    /// TCP data — the cheapest possible DoS.  A single occurrence from a post-handshake peer is
    /// unambiguously malicious (no legitimate node sends >100 MB frames).  Pre-handshake probers
    /// that send an oversized first frame are equally malicious.
    ///
    /// Two occurrences within 120s → `BlockPeer` immediately.  A single occurrence records
    /// `RateLimitPeer` as a gentler first response in case the IP is shared infrastructure.
    pub fn record_frame_bomb(&self, addr: &str) {
        let now = Self::now_secs();
        const FRAME_BOMB_WINDOW_SECS: u64 = 120;

        let count = {
            let mut map = self.frame_bomb_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > FRAME_BOMB_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len()
        };

        if count >= 2 {
            tracing::warn!(
                "🛡️ Frame bomb detected from {} (AV51) — {} oversized frames in {}s — blocking",
                addr,
                count,
                FRAME_BOMB_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::FrameBomb,
                confidence: 0.97,
                severity: AttackSeverity::Critical,
                indicators: vec![
                    format!(
                        "{} crafted oversized-frame headers from {} within {}s (AV51)",
                        count, addr, FRAME_BOMB_WINDOW_SECS
                    ),
                    "4-byte TCP header claiming multi-GB payload — trivial OOM attempt; repeat offender"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::FrameBomb,
                confidence: 0.85,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "Crafted oversized-frame header from {} (AV51) — first occurrence",
                        addr
                    ),
                    "4-byte TCP header claiming multi-GB payload — likely malicious".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Returns true if `ip` has sent at least one oversized frame within the tracking window.
    /// Used by the gossip layer to suppress relaying known frame-bombing peers.
    pub fn is_known_frame_bomber(&self, ip: &str) -> bool {
        let map = self.frame_bomb_times.read();
        map.get(ip).map(|t| !t.is_empty()).unwrap_or(false)
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

    /// Record a collateral hijack attempt from `addr` (AV52).
    ///
    /// This fires when a peer's announced node key does not match either the embedded
    /// masternode key in the collateral UTXO (Level 1) or the on-chain registered operator
    /// pubkey (Level 2).  A legitimate node never announces with a mismatched key — this is
    /// unambiguous evidence that the sender does not own the claimed collateral.
    ///
    /// Thresholds:
    ///   ≥1 in 300 s  → `RateLimitPeer`  (first attempt; allow for misconfigured relays)
    ///   ≥3 in 300 s  → `BlockPeer`      (repeat offender is conducting an active hijack attempt)
    pub fn record_collateral_hijack(&self, addr: &str) {
        let now = Self::now_secs();
        const HIJACK_WINDOW_SECS: u64 = 300;
        const BLOCK_THRESHOLD: usize = 3;

        let count = {
            let mut map = self.collateral_hijack_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > HIJACK_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len()
        };

        if count >= BLOCK_THRESHOLD {
            tracing::warn!(
                "🚨 CollateralHijack from {} (AV52) — {} key-mismatch announcements in {}s — blocking",
                addr,
                count,
                HIJACK_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::CollateralHijack,
                confidence: 0.97,
                severity: AttackSeverity::Critical,
                indicators: vec![
                    format!(
                        "{} collateral key-mismatch announcements from {} within {}s (AV52)",
                        count, addr, HIJACK_WINDOW_SECS
                    ),
                    "Peer's node key does not match collateral UTXO — definitive hijack attempt"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::CollateralHijack,
                confidence: 0.80,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "Collateral key mismatch from {} (AV52) — occurrence {} in {}s window",
                        addr, count, HIJACK_WINDOW_SECS
                    ),
                    "Node key does not match UTXO embedded key or registered operator key"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a zero-value collateral registration attempt from `addr` (AV40).
    ///
    /// This fires when a peer tries to register as a staked tier (Bronze/Silver/Gold)
    /// but the claimed collateral UTXO has a value of zero.  Zero-value UTXOs are trivial
    /// to create and are never legitimate collateral for any staked tier.
    ///
    /// Thresholds:
    ///   ≥1 in 120 s  → `RateLimitPeer`  (first attempt; may be a misconfigured relay)
    ///   ≥2 in 120 s  → `BlockPeer`      (repeat offender is actively polluting the registry)
    pub fn record_zero_collateral_pollution(&self, addr: &str) {
        let now = Self::now_secs();
        const ZERO_COLLATERAL_WINDOW_SECS: u64 = 120;
        const BLOCK_THRESHOLD: usize = 2;

        let count = {
            let mut map = self.zero_collateral_pollution_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > ZERO_COLLATERAL_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len()
        };

        if count >= BLOCK_THRESHOLD {
            tracing::warn!(
                "🛡️ ZeroCollateralPollution from {} (AV40) — {} zero-value registrations in {}s — blocking",
                addr,
                count,
                ZERO_COLLATERAL_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ZeroCollateralPollution,
                confidence: 0.95,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "{} zero-value collateral registrations from {} within {}s (AV40)",
                        count, addr, ZERO_COLLATERAL_WINDOW_SECS
                    ),
                    "Zero-value UTXOs used as staked-tier collateral — registry pollution attack"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else {
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::ZeroCollateralPollution,
                confidence: 0.75,
                severity: AttackSeverity::Medium,
                indicators: vec![
                    format!(
                        "Zero-value collateral registration from {} (AV40) — first occurrence in {}s window",
                        addr, ZERO_COLLATERAL_WINDOW_SECS
                    ),
                    "Staked-tier announcement with 0-value UTXO — never legitimate".to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Record a repeated-invalid-transaction submission from `addr`.
    ///
    /// A single invalid TX is often a benign race condition (stale UTXO, double-spend retry).
    /// After 5 invalid transactions in 60 s the banlist has already issued a violation; this
    /// method gives the AI detector visibility so coordinated campaigns across multiple IPs
    /// can be correlated and the peer can be escalated to `BlockPeer`.
    ///
    /// Thresholds:
    ///   ≥5  in 60 s → `RateLimitPeer`   (suspicious; the banlist 3-strike guard lets some through)
    ///   ≥10 in 60 s → `BlockPeer`        (sustained spam — ban the sender)
    pub fn record_invalid_tx_spam(&self, addr: &str) {
        let now = Self::now_secs();
        const INVALID_TX_WINDOW_SECS: u64 = 60;
        const RATE_LIMIT_THRESHOLD: usize = 5;
        const BLOCK_THRESHOLD: usize = 10;

        let count = {
            let mut map = self.invalid_tx_spam_times.write();
            let timestamps = map.entry(addr.to_string()).or_default();
            while timestamps
                .front()
                .map(|t| now.saturating_sub(*t) > INVALID_TX_WINDOW_SECS)
                .unwrap_or(false)
            {
                timestamps.pop_front();
            }
            timestamps.push_back(now);
            timestamps.len()
        };

        if count >= BLOCK_THRESHOLD {
            tracing::warn!(
                "🚫 InvalidTxSpam from {} — {} invalid TXs in {}s — blocking",
                addr,
                count,
                INVALID_TX_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::InvalidTxSpam,
                confidence: 0.90,
                severity: AttackSeverity::High,
                indicators: vec![
                    format!(
                        "{} invalid transactions from {} within {}s",
                        count, addr, INVALID_TX_WINDOW_SECS
                    ),
                    "Repeated invalid TXs (bad UTXO ref / bad signature) waste validation CPU"
                        .to_string(),
                ],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::BlockPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        } else if count >= RATE_LIMIT_THRESHOLD {
            tracing::debug!(
                "⚠️  InvalidTxSpam from {} — {} invalid TXs in {}s — rate-limiting",
                addr,
                count,
                INVALID_TX_WINDOW_SECS
            );
            self.maybe_add_attack(AttackPattern {
                attack_type: AttackType::InvalidTxSpam,
                confidence: 0.70,
                severity: AttackSeverity::Medium,
                indicators: vec![format!(
                    "{} invalid transactions from {} within {}s",
                    count, addr, INVALID_TX_WINDOW_SECS
                )],
                first_detected: now,
                last_seen: now,
                source_ips: vec![addr.to_string()],
                recommended_action: MitigationAction::RateLimitPeer(addr.to_string()),
                mitigation_applied_at: None,
            });
        }
    }

    /// Return attack patterns whose mitigation action has not yet been applied, and mark them
    /// as applied.  The enforcement loop calls this instead of `get_recent_attacks` so that each
    /// detected attack only triggers one banlist violation — preventing rapid escalation to
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

    #[test]
    fn test_connection_flood_blocks_single_ip_before_subnet() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        for _ in 0..8 {
            detector.record_connection_flood("203.0.113.10");
        }

        let attacks = detector.get_all_attacks();
        assert!(
            attacks.iter().any(|attack| {
                attack.attack_type == AttackType::ConnectionFlood
                    && matches!(
                        &attack.recommended_action,
                        MitigationAction::BlockPeer(ref ip) if ip == "203.0.113.10"
                    )
            }),
            "single noisy IP should escalate to BlockPeer"
        );
        assert!(
            !attacks
                .iter()
                .any(|attack| matches!(&attack.recommended_action, MitigationAction::BanSubnet(_))),
            "single noisy IP must not poison the entire /24"
        );
    }

    #[test]
    fn test_connection_flood_requires_multiple_ips_for_subnet_ban() {
        let dir = tempdir().unwrap();
        let db = Arc::new(sled::open(dir.path()).unwrap());
        let detector = AttackDetector::new(db).unwrap();

        for ip in ["203.0.113.10", "203.0.113.11", "203.0.113.12"] {
            for _ in 0..4 {
                detector.record_connection_flood(ip);
            }
        }

        let attacks = detector.get_all_attacks();
        assert!(attacks.iter().any(|attack| matches!(
            &attack.recommended_action,
            MitigationAction::BanSubnet(ref subnet) if subnet == "203.0.113.0/24"
        )));
    }
}
