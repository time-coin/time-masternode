use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

// Use parking_lot::RwLock instead of std::sync::RwLock
// parking_lot RwLock doesn't poison on panic, making it safer for production
use parking_lot::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AttackType {
    EclipseAttack,      // Peer isolation attempt
    SybilAttack,        // Fake peer flooding
    TimingAttack,       // Clock manipulation
    DoublespendAttack,  // Multiple conflicting transactions
    ForkBombing,        // Intentional fork creation
    ResourceExhaustion, // Memory/bandwidth exhaustion
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
}

#[derive(Debug, Clone)]
struct PeerBehavior {
    _addr: String,
    connect_count: u32,
    disconnect_count: u32,
    invalid_messages: u32,
    fork_count: u32,
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
    _db: Arc<Db>,
    peer_behaviors: Arc<RwLock<HashMap<String, PeerBehavior>>>,
    transaction_history: Arc<RwLock<HashMap<String, TransactionTracker>>>,
    detected_attacks: Arc<RwLock<Vec<AttackPattern>>>,
    _time_window: Duration,
}

impl AttackDetector {
    pub fn new(db: Arc<Db>) -> Result<Self, AppError> {
        Ok(Self {
            _db: db,
            peer_behaviors: Arc::new(RwLock::new(HashMap::new())),
            transaction_history: Arc::new(RwLock::new(HashMap::new())),
            detected_attacks: Arc::new(RwLock::new(Vec::new())),
            _time_window: Duration::from_secs(300), // 5 minute window
        })
    }

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
            fork_count: 0,
            timestamp_drift: Vec::new(),
            first_seen: now,
            last_activity: now,
        });

        behavior.connect_count += 1;
        behavior.last_activity = now;

        // Check for rapid reconnection (Sybil attack indicator)
        if behavior.connect_count > 10 && (now - behavior.first_seen) < 60 {
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

    /// Record fork from peer
    pub fn record_fork(&self, addr: &str) {
        let mut behaviors = self.peer_behaviors.write();
        if let Some(behavior) = behaviors.get_mut(addr) {
            behavior.fork_count += 1;

            // Consistent fork creation is suspicious
            if behavior.fork_count > 5 {
                drop(behaviors);
                self.detect_fork_bombing(addr);
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

    /// Check for eclipse attack (isolated from network)
    pub fn check_eclipse_attack(&self, connected_peer_count: usize, unique_ips: &[String]) -> bool {
        // Eclipse attack indicators:
        // 1. Low peer count
        // 2. All peers from same IP range
        // 3. No diversity in peer connections

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

    /// Detect Sybil attack
    fn detect_sybil_attack(&self, addr: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack = AttackPattern {
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
        };

        self.detected_attacks.write().push(attack);
    }

    /// Detect fork bombing
    fn detect_fork_bombing(&self, addr: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack = AttackPattern {
            attack_type: AttackType::ForkBombing,
            confidence: 0.9,
            severity: AttackSeverity::Critical,
            indicators: vec![
                format!("Consistent fork creation from {}", addr),
                "Intentional chain disruption detected".to_string(),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: vec![addr.to_string()],
            recommended_action: MitigationAction::BlockPeer(addr.to_string()),
        };

        self.detected_attacks.write().push(attack);
    }

    /// Detect timing/clock manipulation attack
    fn detect_timing_attack(&self, addr: &str, avg_drift: i64) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack = AttackPattern {
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
        };

        self.detected_attacks.write().push(attack);
    }

    /// Detect double-spend attempt
    fn detect_doublespend_attempt(&self, txid: &str, sources: Vec<String>) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack = AttackPattern {
            attack_type: AttackType::DoublespendAttack,
            confidence: 0.95,
            severity: AttackSeverity::Critical,
            indicators: vec![
                format!("Conflicting versions of transaction {}", txid),
                format!("Sources: {}", sources.join(", ")),
            ],
            first_detected: now,
            last_seen: now,
            source_ips: sources.clone(),
            recommended_action: MitigationAction::AlertOperator,
        };

        self.detected_attacks.write().push(attack);
    }

    /// Detect eclipse attack
    fn detect_eclipse_attack(&self, peer_ips: &[String]) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack = AttackPattern {
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
        };

        self.detected_attacks.write().push(attack);
    }

    /// Flag malicious peer
    fn flag_malicious_peer(&self, addr: &str) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack = AttackPattern {
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
        };

        self.detected_attacks.write().push(attack);
    }

    /// Get recent attacks
    pub fn get_recent_attacks(&self, since: Duration) -> Vec<AttackPattern> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
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
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cutoff = now.saturating_sub(max_age.as_secs());

        // Clean old peer behaviors
        let mut behaviors = self.peer_behaviors.write();
        behaviors.retain(|_, b| b.last_activity >= cutoff);

        // Clean old transaction history
        let mut history = self.transaction_history.write();
        history.retain(|_, t| t.first_seen >= cutoff);

        // Clean old attacks (keep for longer - 24 hours)
        let attack_cutoff = now.saturating_sub(86400);
        let mut attacks = self.detected_attacks.write();
        attacks.retain(|a| a.last_seen >= attack_cutoff);
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
}
