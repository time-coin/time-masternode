//! Fork resolution system
//!
//! Simple fork resolution: follow the longest valid chain where blocks
//! are not in the future. Peers with the highest valid block height win.

use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{info, warn};

const FORK_HISTORY_KEY: &str = "ai_fork_history";
const PEER_FORK_RELIABILITY_KEY: &str = "ai_peer_fork_reliability";
const MAX_FORK_HISTORY: usize = 1000;
const TIMESTAMP_TOLERANCE_SECS: i64 = 0; // No tolerance for future blocks

/// Parameters for fork resolution
pub struct ForkResolutionParams {
    pub our_height: u64,
    pub our_chain_work: u128,
    pub peer_height: u64,
    pub peer_chain_work: u128,
    pub peer_ip: String,
    pub supporting_peers: Vec<(String, u64, u128)>,
    pub common_ancestor: u64,
    /// Timestamp of peer's tip block (for future-block validation)
    pub peer_tip_timestamp: Option<i64>,
    /// Our tip block hash (for deterministic tiebreaker)
    pub our_tip_hash: Option<[u8; 32]>,
    /// Peer's tip block hash (for deterministic tiebreaker)
    pub peer_tip_hash: Option<[u8; 32]>,
}

/// Fork resolution decision with confidence score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResolution {
    /// Which chain to follow (true = peer's chain, false = our chain)
    pub accept_peer_chain: bool,
    /// Confidence in the decision (0.0 to 1.0)
    pub confidence: f64,
    /// Reasoning for the decision
    pub reasoning: Vec<String>,
    /// Risk assessment
    pub risk_level: RiskLevel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// Historical fork event for learning
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ForkEvent {
    timestamp: u64,
    fork_height: u64,
    our_height: u64,
    peer_height: u64,
    chain_work_diff: i128,
    peer_count_supporting_ours: usize,
    peer_count_supporting_theirs: usize,
    decision: bool,               // What we decided
    outcome: Option<ForkOutcome>, // What actually happened
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ForkOutcome {
    CorrectChoice,
    WrongChoice,
    NetworkSplit,
}

/// Peer reliability in fork scenarios
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerForkReliability {
    peer_ip: String,
    correct_forks: u32,
    incorrect_forks: u32,
    network_splits_caused: u32,
    last_updated: u64,
}

/// AI-powered fork resolution engine
pub struct ForkResolver {
    db: Arc<Db>,
    fork_history: Arc<RwLock<Vec<ForkEvent>>>,
    peer_reliability: Arc<RwLock<HashMap<String, PeerForkReliability>>>,
}

impl ForkResolver {
    pub fn new(db: Arc<Db>) -> Self {
        let fork_history = Self::load_fork_history(&db);
        let peer_reliability = Self::load_peer_reliability(&db);

        Self {
            db,
            fork_history: Arc::new(RwLock::new(fork_history)),
            peer_reliability: Arc::new(RwLock::new(peer_reliability)),
        }
    }

    /// Decide whether to accept a fork - simple rule: highest valid block height wins
    /// A valid block height means the block timestamp is not too far in the future.
    pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution {
        let mut reasoning = Vec::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Rule 1: Check if peer's chain has blocks in the future
        if let Some(peer_timestamp) = params.peer_tip_timestamp {
            if peer_timestamp > now + TIMESTAMP_TOLERANCE_SECS {
                reasoning.push(format!(
                    "REJECT: Peer's tip block timestamp {} is in the future (now: {}, tolerance: {}s)",
                    peer_timestamp, now, TIMESTAMP_TOLERANCE_SECS
                ));
                warn!(
                    "🤖 Fork Resolution: REJECT peer {} - blocks in future (timestamp {} > now {} + {}s)",
                    params.peer_ip, peer_timestamp, now, TIMESTAMP_TOLERANCE_SECS
                );
                return ForkResolution {
                    accept_peer_chain: false,
                    confidence: 1.0,
                    reasoning,
                    risk_level: RiskLevel::High,
                };
            }
        }

        // Rule 2: Highest valid block height wins
        let mut accept = params.peer_height > params.our_height;
        let confidence;

        if params.peer_height > params.our_height {
            // Higher confidence the bigger the height difference
            let diff = params.peer_height - params.our_height;
            confidence = (0.6 + (diff as f64 * 0.1).min(0.4)).min(1.0);
        } else if params.peer_height == params.our_height {
            // SAME HEIGHT - need deterministic tiebreaker
            // Rule 2a: Compare chain work (accumulated difficulty)
            if params.peer_chain_work > params.our_chain_work {
                accept = true;
                confidence = 0.7;
                reasoning.push(format!(
                    "ACCEPT: Same height ({}) but peer has more chain work ({} > {})",
                    params.our_height, params.peer_chain_work, params.our_chain_work
                ));
            } else if params.peer_chain_work < params.our_chain_work {
                accept = false;
                confidence = 0.7;
                reasoning.push(format!(
                    "REJECT: Same height ({}) but our chain has more work ({} > {})",
                    params.our_height, params.our_chain_work, params.peer_chain_work
                ));
            } else {
                // Rule 2b: Chain work is also equal - use tip hash as tiebreaker
                // Lexicographically smallest hash wins (deterministic across all nodes)
                if let (Some(our_hash), Some(peer_hash)) =
                    (params.our_tip_hash, params.peer_tip_hash)
                {
                    // Compare hashes byte by byte
                    match peer_hash.cmp(&our_hash) {
                        std::cmp::Ordering::Less => {
                            // Peer hash is smaller - peer wins
                            accept = true;
                            confidence = 0.9; // High confidence - deterministic
                            reasoning.push(format!(
                                "ACCEPT: Same height ({}), same work, peer hash {} < our hash {} (deterministic tiebreaker)",
                                params.our_height,
                                hex::encode(&peer_hash[..8]),
                                hex::encode(&our_hash[..8])
                            ));
                        }
                        std::cmp::Ordering::Greater => {
                            // Our hash is smaller - we win
                            accept = false;
                            confidence = 0.9; // High confidence - deterministic
                            reasoning.push(format!(
                                "REJECT: Same height ({}), same work, our hash {} < peer hash {} (deterministic tiebreaker)",
                                params.our_height,
                                hex::encode(&our_hash[..8]),
                                hex::encode(&peer_hash[..8])
                            ));
                        }
                        std::cmp::Ordering::Equal => {
                            // Identical blocks - no fork, just keep ours
                            accept = false;
                            confidence = 1.0;
                            reasoning.push(format!(
                                "REJECT: Identical chains at height {} (same hash: {})",
                                params.our_height,
                                hex::encode(&our_hash[..8])
                            ));
                        }
                    }
                } else {
                    // Hashes not available - fall back to keeping our chain
                    accept = false;
                    confidence = 0.5; // Low confidence - not deterministic
                    reasoning.push(format!(
                        "REJECT: Same height ({}), same work, but tip hashes unavailable - keeping our chain",
                        params.our_height
                    ));
                }
            }
        } else {
            // Our chain is longer
            let diff = params.our_height - params.peer_height;
            confidence = (0.6 + (diff as f64 * 0.1).min(0.4)).min(1.0);
        }

        if accept && params.peer_height > params.our_height {
            reasoning.push(format!(
                "ACCEPT: Peer has higher block height ({} > {})",
                params.peer_height, params.our_height
            ));
        } else if !accept && params.our_height > params.peer_height {
            reasoning.push(format!(
                "REJECT: Our chain is longer ({} > {})",
                params.our_height, params.peer_height
            ));
        }

        let risk_level = if params.peer_height.abs_diff(params.our_height) > 100 {
            RiskLevel::High
        } else if params.peer_height.abs_diff(params.our_height) > 10 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        info!(
            "🤖 Fork Resolution: {} peer {} chain (height {} vs ours {}, confidence: {:.0}%)",
            if accept { "ACCEPT" } else { "REJECT" },
            params.peer_ip,
            params.peer_height,
            params.our_height,
            confidence * 100.0
        );

        // Record for history
        self.record_fork_event(
            params.our_height,
            params.peer_height,
            params.our_chain_work as i128 - params.peer_chain_work as i128,
            &params.supporting_peers,
            accept,
        )
        .await;

        ForkResolution {
            accept_peer_chain: accept,
            confidence,
            reasoning,
            risk_level,
        }
    }

    /// Record a fork decision for future learning
    async fn record_fork_event(
        &self,
        our_height: u64,
        peer_height: u64,
        chain_work_diff: i128,
        supporting_peers: &[(String, u64, u128)],
        decision: bool,
    ) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut supporting_ours = 0;
        let mut supporting_theirs = 0;
        for (_, height, _) in supporting_peers {
            if *height == our_height {
                supporting_ours += 1;
            } else if *height == peer_height {
                supporting_theirs += 1;
            }
        }

        let event = ForkEvent {
            timestamp,
            fork_height: our_height.max(peer_height),
            our_height,
            peer_height,
            chain_work_diff,
            peer_count_supporting_ours: supporting_ours,
            peer_count_supporting_theirs: supporting_theirs,
            decision,
            outcome: None,
        };

        let mut history = self.fork_history.write().await;
        history.push(event);

        if history.len() > MAX_FORK_HISTORY {
            history.remove(0);
        }

        if let Ok(encoded) = bincode::serialize(&*history) {
            let _ = self.db.insert(FORK_HISTORY_KEY, encoded);
        }
    }

    /// Update the outcome of a previous fork decision
    pub async fn update_fork_outcome(&self, fork_height: u64, outcome: ForkOutcome) {
        let mut history = self.fork_history.write().await;

        for event in history.iter_mut().rev() {
            if event.fork_height == fork_height && event.outcome.is_none() {
                event.outcome = Some(outcome.clone());

                if let Ok(encoded) = bincode::serialize(&*history) {
                    let _ = self.db.insert(FORK_HISTORY_KEY, encoded);
                }

                info!(
                    "📚 Updated fork outcome at height {}: {:?}",
                    fork_height, outcome
                );
                break;
            }
        }
    }

    /// Update peer reliability based on fork outcomes
    pub async fn update_peer_reliability(
        &self,
        peer_ip: &str,
        was_correct: bool,
        caused_split: bool,
    ) {
        let mut reliability_map = self.peer_reliability.write().await;

        let reliability = reliability_map
            .entry(peer_ip.to_string())
            .or_insert_with(|| PeerForkReliability {
                peer_ip: peer_ip.to_string(),
                correct_forks: 0,
                incorrect_forks: 0,
                network_splits_caused: 0,
                last_updated: 0,
            });

        if was_correct {
            reliability.correct_forks += 1;
        } else {
            reliability.incorrect_forks += 1;
        }

        if caused_split {
            reliability.network_splits_caused += 1;
        }

        reliability.last_updated = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if let Ok(encoded) = bincode::serialize(&*reliability_map) {
            let _ = self.db.insert(PEER_FORK_RELIABILITY_KEY, encoded);
        }
    }

    fn load_fork_history(db: &Db) -> Vec<ForkEvent> {
        db.get(FORK_HISTORY_KEY)
            .ok()
            .flatten()
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_default()
    }

    fn load_peer_reliability(db: &Db) -> HashMap<String, PeerForkReliability> {
        db.get(PEER_FORK_RELIABILITY_KEY)
            .ok()
            .flatten()
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_default()
    }

    /// Get statistics about fork resolution performance
    pub async fn get_statistics(&self) -> ForkResolverStats {
        let history = self.fork_history.read().await;
        let reliability = self.peer_reliability.read().await;

        let total_forks = history.len();
        let mut correct_decisions = 0;
        let mut wrong_decisions = 0;
        let mut network_splits = 0;

        for event in history.iter() {
            if let Some(outcome) = &event.outcome {
                match outcome {
                    ForkOutcome::CorrectChoice => correct_decisions += 1,
                    ForkOutcome::WrongChoice => wrong_decisions += 1,
                    ForkOutcome::NetworkSplit => network_splits += 1,
                }
            }
        }

        let total_reliability_entries = reliability.len();
        let avg_peer_success_rate = if !reliability.is_empty() {
            reliability
                .values()
                .map(|r| {
                    let total = r.correct_forks + r.incorrect_forks;
                    if total > 0 {
                        r.correct_forks as f64 / total as f64
                    } else {
                        0.0
                    }
                })
                .sum::<f64>()
                / reliability.len() as f64
        } else {
            0.0
        };

        ForkResolverStats {
            total_forks,
            correct_decisions,
            wrong_decisions,
            network_splits,
            pending_outcomes: total_forks - correct_decisions - wrong_decisions - network_splits,
            total_peers_tracked: total_reliability_entries,
            avg_peer_success_rate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResolverStats {
    pub total_forks: usize,
    pub correct_decisions: usize,
    pub wrong_decisions: usize,
    pub network_splits: usize,
    pub pending_outcomes: usize,
    pub total_peers_tracked: usize,
    pub avg_peer_success_rate: f64,
}
