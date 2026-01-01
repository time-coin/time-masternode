//! AI-powered fork resolution system
//!
//! This module uses machine learning to intelligently resolve blockchain forks
//! by learning from historical patterns, network consensus, and peer behavior.

use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info};

const FORK_HISTORY_KEY: &str = "ai_fork_history";
const PEER_FORK_RELIABILITY_KEY: &str = "ai_peer_fork_reliability";
const MAX_FORK_HISTORY: usize = 1000;

/// Parameters for fork resolution
pub struct ForkResolutionParams {
    pub our_height: u64,
    pub our_chain_work: u128,
    pub peer_height: u64,
    pub peer_chain_work: u128,
    pub peer_ip: String,
    pub supporting_peers: Vec<(String, u64, u128)>,
    pub common_ancestor: u64,
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

    /// Decide whether to accept a fork using AI-based analysis
    pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution {
        let mut reasoning = Vec::new();
        let mut confidence = 0.0;
        let mut risk_score = 0.0;

        // Factor 1: Chain work difference (40% weight)
        let work_factor = self.analyze_chain_work(
            params.our_chain_work,
            params.peer_chain_work,
            &mut reasoning,
        );
        confidence += work_factor * 0.4;

        // Factor 2: Network consensus (30% weight)
        let consensus_factor = self
            .analyze_network_consensus(
                params.our_height,
                params.peer_height,
                &params.supporting_peers,
                &mut reasoning,
            )
            .await;
        confidence += consensus_factor * 0.3;

        // Factor 3: Peer reliability (15% weight)
        let reliability_factor = self
            .analyze_peer_reliability(&params.peer_ip, &mut reasoning)
            .await;
        confidence += reliability_factor * 0.15;

        // Factor 4: Historical patterns (10% weight)
        let pattern_factor = self
            .analyze_historical_patterns(
                params.our_height,
                params.peer_height,
                params.our_chain_work,
                params.peer_chain_work,
                &mut reasoning,
            )
            .await;
        confidence += pattern_factor * 0.1;

        // Factor 5: Fork depth analysis (5% weight)
        let depth_factor = self.analyze_fork_depth(
            params.common_ancestor,
            params.our_height,
            params.peer_height,
            &mut reasoning,
            &mut risk_score,
        );
        confidence += depth_factor * 0.05;

        // Determine risk level
        let risk_level = if risk_score > 0.75 {
            RiskLevel::Critical
        } else if risk_score > 0.5 {
            RiskLevel::High
        } else if risk_score > 0.25 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        // Make decision
        let accept_peer_chain = confidence > 0.5;

        // Log the decision
        info!(
            "🤖 AI Fork Resolution: {} peer chain (confidence: {:.2}%, risk: {:?})",
            if accept_peer_chain {
                "ACCEPT"
            } else {
                "REJECT"
            },
            confidence * 100.0,
            risk_level
        );
        for reason in &reasoning {
            debug!("  📋 {}", reason);
        }

        // Record the event for learning
        self.record_fork_event(
            params.our_height,
            params.peer_height,
            params.our_chain_work as i128 - params.peer_chain_work as i128,
            &params.supporting_peers,
            accept_peer_chain,
        )
        .await;

        ForkResolution {
            accept_peer_chain,
            confidence,
            reasoning,
            risk_level,
        }
    }

    /// Analyze chain work difference
    fn analyze_chain_work(
        &self,
        our_work: u128,
        peer_work: u128,
        reasoning: &mut Vec<String>,
    ) -> f64 {
        let work_diff = peer_work as i128 - our_work as i128;
        let work_ratio = if our_work > 0 {
            peer_work as f64 / our_work as f64
        } else {
            1.0
        };

        if work_diff > 0 {
            reasoning.push(format!(
                "Peer has more chain work (+{}, ratio: {:.2})",
                work_diff, work_ratio
            ));
            // More work = higher confidence in peer chain
            (work_ratio - 1.0).min(1.0)
        } else if work_diff < 0 {
            reasoning.push(format!(
                "We have more chain work (+{}, ratio: {:.2})",
                -work_diff,
                1.0 / work_ratio
            ));
            // We have more work = low confidence in peer chain
            0.0
        } else {
            reasoning.push("Equal chain work".to_string());
            0.5
        }
    }

    /// Analyze network consensus
    async fn analyze_network_consensus(
        &self,
        our_height: u64,
        peer_height: u64,
        supporting_peers: &[(String, u64, u128)],
        reasoning: &mut Vec<String>,
    ) -> f64 {
        let mut supporting_ours = 0;
        let mut supporting_theirs = 0;
        let mut total_work_ours = 0u128;
        let mut total_work_theirs = 0u128;

        for (_, height, work) in supporting_peers {
            if *height == our_height {
                supporting_ours += 1;
                total_work_ours += work;
            } else if *height == peer_height {
                supporting_theirs += 1;
                total_work_theirs += work;
            }
        }

        let total_peers = supporting_ours + supporting_theirs;
        if total_peers == 0 {
            reasoning.push("No peer consensus data available".to_string());
            return 0.5;
        }

        let peer_consensus = supporting_theirs as f64 / total_peers as f64;
        let work_consensus = if total_work_ours + total_work_theirs > 0 {
            total_work_theirs as f64 / (total_work_ours + total_work_theirs) as f64
        } else {
            0.5
        };

        reasoning.push(format!(
            "Network consensus: {} of {} peers support peer chain ({:.1}%)",
            supporting_theirs,
            total_peers,
            peer_consensus * 100.0
        ));
        reasoning.push(format!(
            "Work consensus: {:.1}% of total work supports peer chain",
            work_consensus * 100.0
        ));

        // Average of peer count and work consensus
        (peer_consensus + work_consensus) / 2.0
    }

    /// Analyze peer reliability
    async fn analyze_peer_reliability(&self, peer_ip: &str, reasoning: &mut Vec<String>) -> f64 {
        let reliability_map = self.peer_reliability.read().await;

        if let Some(reliability) = reliability_map.get(peer_ip) {
            let total = reliability.correct_forks + reliability.incorrect_forks;
            if total == 0 {
                reasoning.push(format!("Peer {} has no fork history", peer_ip));
                return 0.5;
            }

            let success_rate = reliability.correct_forks as f64 / total as f64;

            // Penalize peers that caused network splits
            let split_penalty = reliability.network_splits_caused as f64 * 0.1;
            let adjusted_rate = (success_rate - split_penalty).max(0.0);

            reasoning.push(format!(
                "Peer {} has {:.1}% fork success rate ({}/{} correct, {} splits)",
                peer_ip,
                success_rate * 100.0,
                reliability.correct_forks,
                total,
                reliability.network_splits_caused
            ));

            adjusted_rate
        } else {
            reasoning.push(format!("Peer {} is new, no reliability data", peer_ip));
            0.5 // Neutral for new peers
        }
    }

    /// Analyze historical patterns
    async fn analyze_historical_patterns(
        &self,
        our_height: u64,
        peer_height: u64,
        our_work: u128,
        peer_work: u128,
        reasoning: &mut Vec<String>,
    ) -> f64 {
        let history = self.fork_history.read().await;

        if history.is_empty() {
            reasoning.push("No historical fork data available".to_string());
            return 0.5;
        }

        // Find similar historical events
        let height_diff = peer_height as i64 - our_height as i64;
        let work_diff = peer_work as i128 - our_work as i128;

        let mut similar_events = Vec::new();
        for event in history.iter() {
            let hist_height_diff = event.peer_height as i64 - event.our_height as i64;
            let hist_work_diff = event.chain_work_diff;

            // Consider events with similar characteristics
            if (hist_height_diff - height_diff).abs() <= 10
                && (hist_work_diff - work_diff).abs() <= (work_diff / 10).abs()
            {
                similar_events.push(event);
            }
        }

        if similar_events.is_empty() {
            reasoning.push("No similar historical forks found".to_string());
            return 0.5;
        }

        // Calculate success rate of accepting peer chain in similar scenarios
        let mut correct_decisions = 0;
        let mut total_decisions = 0;

        for event in &similar_events {
            if let Some(outcome) = &event.outcome {
                total_decisions += 1;
                match outcome {
                    ForkOutcome::CorrectChoice if event.decision => correct_decisions += 1,
                    ForkOutcome::WrongChoice if !event.decision => correct_decisions += 1,
                    _ => {}
                }
            }
        }

        if total_decisions == 0 {
            reasoning.push(format!(
                "Found {} similar forks, but no outcome data",
                similar_events.len()
            ));
            return 0.5;
        }

        let success_rate = correct_decisions as f64 / total_decisions as f64;
        reasoning.push(format!(
            "Historical analysis: {} similar forks, {:.1}% success rate accepting peer",
            similar_events.len(),
            success_rate * 100.0
        ));

        success_rate
    }

    /// Analyze fork depth for risk assessment
    fn analyze_fork_depth(
        &self,
        common_ancestor: u64,
        our_height: u64,
        peer_height: u64,
        reasoning: &mut Vec<String>,
        risk_score: &mut f64,
    ) -> f64 {
        let our_depth = our_height - common_ancestor;
        let peer_depth = peer_height - common_ancestor;
        let max_depth = our_depth.max(peer_depth);

        reasoning.push(format!(
            "Fork depth: {} blocks (our: {}, peer: {})",
            max_depth, our_depth, peer_depth
        ));

        // Deep forks are risky
        if max_depth > 100 {
            *risk_score += 0.5;
            reasoning.push("⚠️ Deep fork detected (>100 blocks)".to_string());
        } else if max_depth > 50 {
            *risk_score += 0.3;
            reasoning.push("⚠️ Moderate fork depth (>50 blocks)".to_string());
        }

        // Prefer the longer chain
        if peer_depth > our_depth {
            0.7
        } else if our_depth > peer_depth {
            0.3
        } else {
            0.5
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
            outcome: None, // Will be updated later when outcome is known
        };

        let mut history = self.fork_history.write().await;
        history.push(event.clone());

        // Keep only recent history
        if history.len() > MAX_FORK_HISTORY {
            history.remove(0);
        }

        // Persist to database
        if let Ok(encoded) = bincode::serialize(&*history) {
            let _ = self.db.insert(FORK_HISTORY_KEY, encoded);
        }
    }

    /// Update the outcome of a previous fork decision (for learning)
    pub async fn update_fork_outcome(&self, fork_height: u64, outcome: ForkOutcome) {
        let mut history = self.fork_history.write().await;

        // Find the most recent fork at this height
        for event in history.iter_mut().rev() {
            if event.fork_height == fork_height && event.outcome.is_none() {
                event.outcome = Some(outcome.clone());

                // Persist to database
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

        // Persist to database
        if let Ok(encoded) = bincode::serialize(&*reliability_map) {
            let _ = self.db.insert(PEER_FORK_RELIABILITY_KEY, encoded);
        }
    }

    /// Load fork history from database
    fn load_fork_history(db: &Db) -> Vec<ForkEvent> {
        db.get(FORK_HISTORY_KEY)
            .ok()
            .flatten()
            .and_then(|data| bincode::deserialize(&data).ok())
            .unwrap_or_default()
    }

    /// Load peer reliability from database
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
