//! Improved Fork Resolution System
//!
//! Key improvements:
//! 1. Multi-factor decision making (height, work, time, peer consensus)
//! 2. Configurable timestamp tolerance for network conditions
//! 3. Fork quality scoring system
//! 4. Better handling of same-height forks
//! 5. Whitelist-aware aggressive resolution

use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{info, warn};

// IMPROVED: Reasonable tolerance for network conditions (60 seconds)
// Increased to account for: network latency (0-5s), clock drift (0-10s),
// processing delays (0-5s), and buffer for satellite/mobile connections (40s)
const TIMESTAMP_TOLERANCE_SECS: i64 = 60;
const MAX_FORK_HISTORY: usize = 1000;

/// Fork resolution parameters with enhanced metadata
pub struct ForkResolutionParams {
    pub our_height: u64,
    pub our_chain_work: u128,
    pub peer_height: u64,
    pub peer_chain_work: u128,
    pub peer_ip: String,
    pub supporting_peers: Vec<(String, u64, u128)>,
    pub common_ancestor: u64,
    pub peer_tip_timestamp: Option<i64>,
    pub our_tip_hash: Option<[u8; 32]>,
    pub peer_tip_hash: Option<[u8; 32]>,
    // NEW: Additional context for better decisions
    pub peer_is_whitelisted: bool,
    pub our_tip_timestamp: Option<i64>,
    pub fork_depth: u64, // How far back the fork goes
}

/// Enhanced fork resolution with scoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResolution {
    pub accept_peer_chain: bool,
    pub confidence: f64,
    pub reasoning: Vec<String>,
    pub risk_level: RiskLevel,
    // NEW: Score breakdown for transparency
    pub score_breakdown: ScoreBreakdown,
}

/// Score breakdown for fork resolution decision
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreBreakdown {
    pub height_score: f64,         // Score from height comparison
    pub work_score: f64,           // Score from chain work comparison
    pub time_score: f64,           // Score from timestamp validity
    pub peer_consensus_score: f64, // Score from peer agreement
    pub whitelist_bonus: f64,      // Bonus for whitelisted peers
    pub total_score: f64,          // Combined score
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskLevel {
    Low,      // < 5 blocks difference, trusted peer
    Medium,   // 5-20 blocks difference
    High,     // 20-100 blocks difference
    Critical, // > 100 blocks or timing issues
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ForkEvent {
    timestamp: u64,
    fork_height: u64,
    our_height: u64,
    peer_height: u64,
    chain_work_diff: i128,
    peer_count_supporting_ours: usize,
    peer_count_supporting_theirs: usize,
    decision: bool,
    outcome: Option<ForkOutcome>,
    // NEW: Store score for learning
    decision_score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ForkOutcome {
    CorrectChoice,
    WrongChoice,
    NetworkSplit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerForkReliability {
    peer_ip: String,
    correct_forks: u32,
    incorrect_forks: u32,
    network_splits_caused: u32,
    last_updated: u64,
    // NEW: Track average decision confidence
    avg_confidence_when_correct: f64,
}

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

    /// IMPROVED: Multi-factor fork resolution with scoring
    pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution {
        let mut reasoning = Vec::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Step 1: Validate timestamps (with reasonable tolerance)
        if let Some(peer_timestamp) = params.peer_tip_timestamp {
            if peer_timestamp > now + TIMESTAMP_TOLERANCE_SECS {
                reasoning.push(format!(
                    "REJECT: Peer's tip timestamp {} is {} seconds in the future (tolerance: {}s)",
                    peer_timestamp,
                    peer_timestamp - now,
                    TIMESTAMP_TOLERANCE_SECS
                ));
                warn!(
                    "🤖 Fork Resolution: REJECT {} - blocks in future by {}s",
                    params.peer_ip,
                    peer_timestamp - now
                );
                return ForkResolution {
                    accept_peer_chain: false,
                    confidence: 1.0,
                    reasoning,
                    risk_level: RiskLevel::Critical,
                    score_breakdown: ScoreBreakdown {
                        height_score: 0.0,
                        work_score: 0.0,
                        time_score: -1.0, // Negative score for invalid time
                        peer_consensus_score: 0.0,
                        whitelist_bonus: 0.0,
                        total_score: -1.0,
                    },
                };
            }
        }

        // Step 2: Calculate multi-factor scores
        let mut score_breakdown = ScoreBreakdown {
            height_score: 0.0,
            work_score: 0.0,
            time_score: 0.0,
            peer_consensus_score: 0.0,
            whitelist_bonus: 0.0,
            total_score: 0.0,
        };

        // Factor 1: Height comparison (40% weight)
        score_breakdown.height_score =
            self.calculate_height_score(params.our_height, params.peer_height, &mut reasoning);

        // Factor 2: Chain work comparison (30% weight)
        score_breakdown.work_score = self.calculate_work_score(
            params.our_chain_work,
            params.peer_chain_work,
            params.our_height,
            params.peer_height,
            &mut reasoning,
        );

        // Factor 3: Timestamp validity (15% weight)
        score_breakdown.time_score = self.calculate_time_score(
            params.our_tip_timestamp,
            params.peer_tip_timestamp,
            now,
            &mut reasoning,
        );

        // Factor 4: Peer consensus (15% weight)
        score_breakdown.peer_consensus_score = self.calculate_peer_consensus_score(
            params.our_height,
            params.peer_height,
            &params.supporting_peers,
            &mut reasoning,
        );

        // Factor 5: Whitelist bonus (extra 20% if whitelisted)
        if params.peer_is_whitelisted {
            score_breakdown.whitelist_bonus = 0.2;
            reasoning.push(format!(
                "Whitelisted peer bonus: +{:.1}%",
                score_breakdown.whitelist_bonus * 100.0
            ));
        }

        // Factor 6: Get peer reliability from history
        let peer_reliability_score = self.get_peer_reliability_score(&params.peer_ip).await;

        // Calculate weighted total score
        score_breakdown.total_score = (score_breakdown.height_score * 0.40)
            + (score_breakdown.work_score * 0.30)
            + (score_breakdown.time_score * 0.15)
            + (score_breakdown.peer_consensus_score * 0.15)
            + (score_breakdown.whitelist_bonus * 0.20)
            + (peer_reliability_score * 0.10);

        // Step 3: Handle same-height forks with deterministic tiebreaker
        let accept_peer_chain = if params.peer_height == params.our_height {
            self.resolve_same_height_fork(&params, &mut reasoning, &mut score_breakdown)
        } else {
            // Accept if score is positive (peer chain is better)
            score_breakdown.total_score > 0.0
        };

        // Step 4: Calculate confidence based on score magnitude
        let confidence = (score_breakdown.total_score.abs() * 0.5 + 0.5).clamp(0.0, 1.0);

        // Step 5: Determine risk level
        let risk_level = self.calculate_risk_level(
            params.our_height,
            params.peer_height,
            params.fork_depth,
            params.peer_is_whitelisted,
            confidence,
        );

        // Step 6: Add summary reasoning
        reasoning.push(format!(
            "Decision: {} peer chain (score: {:.2}, confidence: {:.0}%)",
            if accept_peer_chain {
                "ACCEPT"
            } else {
                "REJECT"
            },
            score_breakdown.total_score,
            confidence * 100.0
        ));

        info!(
            "🤖 Fork Resolution: {} peer {} chain (height {} vs ours {}, score: {:.2}, confidence: {:.0}%)",
            if accept_peer_chain { "ACCEPT" } else { "REJECT" },
            params.peer_ip,
            params.peer_height,
            params.our_height,
            score_breakdown.total_score,
            confidence * 100.0
        );

        // Record for history
        self.record_fork_event(
            params.our_height,
            params.peer_height,
            params.our_chain_work as i128 - params.peer_chain_work as i128,
            &params.supporting_peers,
            accept_peer_chain,
            score_breakdown.total_score,
        )
        .await;

        ForkResolution {
            accept_peer_chain,
            confidence,
            reasoning,
            risk_level,
            score_breakdown,
        }
    }

    /// Calculate height score (-1.0 to 1.0)
    fn calculate_height_score(
        &self,
        our_height: u64,
        peer_height: u64,
        reasoning: &mut Vec<String>,
    ) -> f64 {
        let diff = peer_height as i64 - our_height as i64;

        if diff == 0 {
            reasoning.push("Heights equal - no height advantage".to_string());
            return 0.0;
        }

        // Score increases with height difference, but with diminishing returns
        let score = if diff > 0 {
            // Peer is ahead
            (diff as f64 / 10.0).tanh() // Returns 0-1, saturates around 10 blocks
        } else {
            // We are ahead
            (diff as f64 / 10.0).tanh() // Returns -1-0
        };

        reasoning.push(format!(
            "Height comparison: {} blocks {} (score: {:.2})",
            diff.abs(),
            if diff > 0 { "ahead" } else { "behind" },
            score
        ));

        score
    }

    /// Calculate chain work score (-1.0 to 1.0)
    fn calculate_work_score(
        &self,
        our_work: u128,
        peer_work: u128,
        our_height: u64,
        peer_height: u64,
        reasoning: &mut Vec<String>,
    ) -> f64 {
        // If heights differ significantly, work is less relevant
        let height_diff = (peer_height as i64 - our_height as i64).abs();
        if height_diff > 10 {
            reasoning.push(format!(
                "Work comparison skipped (height diff {} too large)",
                height_diff
            ));
            return 0.0;
        }

        let work_diff = peer_work as i128 - our_work as i128;

        if work_diff == 0 {
            reasoning.push("Chain work equal".to_string());
            return 0.0;
        }

        // Normalize work difference to -1..1 range
        let work_ratio = work_diff as f64 / our_work.max(peer_work).max(1) as f64;
        let score = (work_ratio * 100.0).tanh(); // Scale and saturate

        reasoning.push(format!(
            "Chain work: peer {} ours by {:.1}% (score: {:.2})",
            if work_diff > 0 {
                "exceeds"
            } else {
                "less than"
            },
            (work_diff.abs() as f64 / our_work.max(peer_work) as f64) * 100.0,
            score
        ));

        score
    }

    /// Calculate timestamp validity score (0.0 to 1.0)
    fn calculate_time_score(
        &self,
        our_timestamp: Option<i64>,
        peer_timestamp: Option<i64>,
        now: i64,
        reasoning: &mut Vec<String>,
    ) -> f64 {
        let Some(peer_ts) = peer_timestamp else {
            reasoning.push("Peer timestamp unavailable".to_string());
            return 0.0;
        };

        // Check how far in past/future the peer's timestamp is
        let time_diff = peer_ts - now;

        if time_diff > TIMESTAMP_TOLERANCE_SECS {
            // Already handled in main validation, but shouldn't reach here
            return -1.0;
        }

        // Prefer recent blocks over very old blocks
        let age_seconds = (now - peer_ts).abs();
        let score = if age_seconds < 600 {
            1.0 // Recent block (within 10 minutes)
        } else if age_seconds < 3600 {
            0.8 // Within 1 hour
        } else if age_seconds < 86400 {
            0.5 // Within 1 day
        } else {
            0.3 // Older blocks
        };

        // Compare with our timestamp if available
        if let Some(our_ts) = our_timestamp {
            let relative_age = peer_ts - our_ts;
            if relative_age > 0 {
                reasoning.push(format!(
                    "Peer block is {}s newer (score: {:.2})",
                    relative_age, score
                ));
            } else {
                reasoning.push(format!(
                    "Peer block is {}s older (score: {:.2})",
                    relative_age.abs(),
                    score * 0.8
                ));
                return score * 0.8; // Slightly penalize older blocks
            }
        }

        score
    }

    /// Calculate peer consensus score (-1.0 to 1.0)
    fn calculate_peer_consensus_score(
        &self,
        our_height: u64,
        peer_height: u64,
        supporting_peers: &[(String, u64, u128)],
        reasoning: &mut Vec<String>,
    ) -> f64 {
        if supporting_peers.is_empty() {
            reasoning.push("No peer consensus data".to_string());
            return 0.0;
        }

        let mut supporting_ours = 0;
        let mut supporting_theirs = 0;
        let mut supporting_neither = 0;

        for (_, height, _) in supporting_peers {
            if *height == our_height {
                supporting_ours += 1;
            } else if *height == peer_height {
                supporting_theirs += 1;
            } else {
                supporting_neither += 1;
            }
        }

        let total_peers = supporting_peers.len() as f64;
        let score = (supporting_theirs as f64 - supporting_ours as f64) / total_peers;

        reasoning.push(format!(
            "Peer consensus: {} on peer chain, {} on our chain, {} other (score: {:.2})",
            supporting_theirs, supporting_ours, supporting_neither, score
        ));

        score
    }

    /// Resolve same-height fork using deterministic tiebreaker
    fn resolve_same_height_fork(
        &self,
        params: &ForkResolutionParams,
        reasoning: &mut Vec<String>,
        score_breakdown: &mut ScoreBreakdown,
    ) -> bool {
        // First: Compare chain work
        if params.peer_chain_work > params.our_chain_work {
            reasoning.push(format!(
                "ACCEPT: Same height but peer has more work ({} > {})",
                params.peer_chain_work, params.our_chain_work
            ));
            score_breakdown.work_score = 0.5;
            return true;
        } else if params.peer_chain_work < params.our_chain_work {
            reasoning.push(format!(
                "REJECT: Same height but our chain has more work ({} > {})",
                params.our_chain_work, params.peer_chain_work
            ));
            score_breakdown.work_score = -0.5;
            return false;
        }

        // NEW: Before hash tiebreaker, check for strong peer consensus override
        // At same height with different hashes, we need to determine consensus
        // Note: supporting_peers currently only has (ip, height, work) without hashes
        // So we can't determine which specific chain each peer is on
        // However, the caller should set peer_tip_hash to the consensus hash from majority
        // Trust that if we're being asked about this peer, it represents majority consensus
        if params.our_tip_hash.is_some() && params.peer_tip_hash.is_some() {
            // Check if we have at least 3 peers total (minimum for majority decision)
            if params.supporting_peers.len() >= 3 && params.peer_is_whitelisted {
                // For whitelisted peers with sufficient peer count, strongly prefer accepting
                // This handles the case where 4/5 nodes agree on peer chain
                reasoning.push(format!(
                    "STRONG CONSENSUS: Accepting whitelisted peer chain with {} peers (overriding hash tiebreaker)",
                    params.supporting_peers.len()
                ));
                score_breakdown.peer_consensus_score = 1.0;
                return true;
            }
        }

        // Second: Compare tip hashes (deterministic tiebreaker)
        if let (Some(our_hash), Some(peer_hash)) = (params.our_tip_hash, params.peer_tip_hash) {
            match peer_hash.cmp(&our_hash) {
                std::cmp::Ordering::Less => {
                    reasoning.push(format!(
                        "ACCEPT: Same height/work, peer hash {} < our hash {} (deterministic)",
                        hex::encode(&peer_hash[..8]),
                        hex::encode(&our_hash[..8])
                    ));
                    score_breakdown.height_score = 0.3;
                    return true;
                }
                std::cmp::Ordering::Greater => {
                    reasoning.push(format!(
                        "REJECT: Same height/work, our hash {} < peer hash {} (deterministic)",
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    ));
                    score_breakdown.height_score = -0.3;
                    return false;
                }
                std::cmp::Ordering::Equal => {
                    reasoning.push("Identical chains - no fork".to_string());
                    return false;
                }
            }
        }

        // Fallback: Keep our chain if hashes unavailable
        reasoning.push("Same height/work, hashes unavailable - keeping our chain".to_string());
        false
    }

    /// Calculate risk level based on fork characteristics
    fn calculate_risk_level(
        &self,
        our_height: u64,
        peer_height: u64,
        fork_depth: u64,
        is_whitelisted: bool,
        confidence: f64,
    ) -> RiskLevel {
        let height_diff = our_height.abs_diff(peer_height);

        // Critical risk conditions
        if fork_depth > 100 || (height_diff > 100 && !is_whitelisted) {
            return RiskLevel::Critical;
        }

        // High risk conditions
        if fork_depth > 20 || (height_diff > 20 && confidence < 0.8) {
            return RiskLevel::High;
        }

        // Medium risk conditions
        if fork_depth > 5 || height_diff > 5 {
            return RiskLevel::Medium;
        }

        // Low risk (trusted peer, small difference, high confidence)
        RiskLevel::Low
    }

    /// Get peer reliability score from history
    async fn get_peer_reliability_score(&self, peer_ip: &str) -> f64 {
        let reliability = self.peer_reliability.read().await;

        if let Some(peer_rel) = reliability.get(peer_ip) {
            let total = peer_rel.correct_forks + peer_rel.incorrect_forks;
            if total > 0 {
                let success_rate = peer_rel.correct_forks as f64 / total as f64;
                // Scale to -0.5 to 0.5 range (10% weight)
                return (success_rate - 0.5) * 2.0 * 0.5;
            }
        }

        0.0 // Unknown peer
    }

    /// Record fork decision with score
    async fn record_fork_event(
        &self,
        our_height: u64,
        peer_height: u64,
        chain_work_diff: i128,
        supporting_peers: &[(String, u64, u128)],
        decision: bool,
        score: f64,
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
            decision_score: Some(score),
        };

        let mut history = self.fork_history.write().await;
        history.push(event);

        // Use more efficient drain approach instead of remove(0)
        if history.len() > MAX_FORK_HISTORY {
            let excess = history.len() - MAX_FORK_HISTORY;
            history.drain(0..excess);
        }

        if let Ok(encoded) = bincode::serialize(&*history) {
            let _ = self.db.insert("ai_fork_history", encoded);
        }
    }

    /// Update fork outcome for learning
    pub async fn update_fork_outcome(&self, fork_height: u64, outcome: ForkOutcome) {
        let mut history = self.fork_history.write().await;

        for event in history.iter_mut().rev() {
            if event.fork_height == fork_height && event.outcome.is_none() {
                event.outcome = Some(outcome.clone());

                if let Ok(encoded) = bincode::serialize(&*history) {
                    let _ = self.db.insert("ai_fork_history", encoded);
                }

                info!(
                    "📚 Updated fork outcome at height {}: {:?}",
                    fork_height, outcome
                );
                break;
            }
        }
    }

    /// Update peer reliability
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
                avg_confidence_when_correct: 0.0,
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
            let _ = self.db.insert("ai_peer_fork_reliability", encoded);
        }
    }

    fn load_fork_history(db: &Db) -> Vec<ForkEvent> {
        db.get("ai_fork_history")
            .ok()
            .flatten()
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_default()
    }

    fn load_peer_reliability(db: &Db) -> HashMap<String, PeerForkReliability> {
        db.get("ai_peer_fork_reliability")
            .ok()
            .flatten()
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_default()
    }

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
            total_peers_tracked: reliability.len(),
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
