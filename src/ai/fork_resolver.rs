//! Simplified Fork Resolution System
//!
//! Simple rule: Longest valid chain wins.
//! - If peer has longer valid chain → accept
//! - If same length → use block hash tiebreaker (lexicographic)
//! - Validate timestamps are not in future (with tolerance)

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

// Timestamp tolerance for network conditions (60 seconds)
// Accounts for: network latency, clock drift, processing delays
const TIMESTAMP_TOLERANCE_SECS: i64 = 60;

/// Simplified fork resolution parameters
pub struct ForkResolutionParams {
    pub our_height: u64,
    pub peer_height: u64,
    pub peer_ip: String,
    pub peer_tip_timestamp: Option<i64>,
    pub our_tip_hash: Option<[u8; 32]>,
    pub peer_tip_hash: Option<[u8; 32]>,
}

/// Simple fork resolution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResolution {
    pub accept_peer_chain: bool,
    pub reasoning: Vec<String>,
}

/// Fork resolver with simple longest-chain rule
pub struct ForkResolver;

impl ForkResolver {
    pub fn new(_db: std::sync::Arc<sled::Db>) -> Self {
        Self
    }

    /// Simple fork resolution: longest valid chain wins
    pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution {
        let mut reasoning = Vec::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Step 1: Validate timestamp (blocks must not be in future)
        if let Some(peer_timestamp) = params.peer_tip_timestamp {
            if peer_timestamp > now + TIMESTAMP_TOLERANCE_SECS {
                reasoning.push(format!(
                    "REJECT: Peer's tip timestamp {} is {} seconds in the future (tolerance: {}s)",
                    peer_timestamp,
                    peer_timestamp - now,
                    TIMESTAMP_TOLERANCE_SECS
                ));
                warn!(
                    "❌ Fork Resolution: REJECT {} - blocks {} seconds in future",
                    params.peer_ip,
                    peer_timestamp - now
                );
                return ForkResolution {
                    accept_peer_chain: false,
                    reasoning,
                };
            }
        }

        // Step 2: Simple height comparison
        let accept_peer_chain = if params.peer_height > params.our_height {
            reasoning.push(format!(
                "ACCEPT: Peer has longer chain ({} > {})",
                params.peer_height, params.our_height
            ));
            info!(
                "✅ Fork Resolution: ACCEPT peer {} - longer chain (height {} > ours {})",
                params.peer_ip, params.peer_height, params.our_height
            );
            true
        } else if params.peer_height < params.our_height {
            reasoning.push(format!(
                "REJECT: Our chain is longer ({} > {})",
                params.our_height, params.peer_height
            ));
            info!(
                "❌ Fork Resolution: REJECT peer {} - our chain is longer (height {} > theirs {})",
                params.peer_ip, params.our_height, params.peer_height
            );
            false
        } else {
            // Step 3: Same height - use deterministic tiebreaker (hash comparison)
            if let (Some(our_hash), Some(peer_hash)) = (params.our_tip_hash, params.peer_tip_hash) {
                if peer_hash == our_hash {
                    reasoning.push("No fork: identical chains".to_string());
                    info!("✅ No fork: peer {} has identical chain", params.peer_ip);
                    false
                } else if peer_hash < our_hash {
                    reasoning.push(format!(
                        "ACCEPT: Same height, peer hash {} < our hash {} (deterministic tiebreaker)",
                        hex::encode(&peer_hash[..8]),
                        hex::encode(&our_hash[..8])
                    ));
                    info!(
                        "✅ Fork Resolution: ACCEPT peer {} - lower hash wins (height {}, peer {} < ours {})",
                        params.peer_ip,
                        params.peer_height,
                        hex::encode(&peer_hash[..8]),
                        hex::encode(&our_hash[..8])
                    );
                    true
                } else {
                    reasoning.push(format!(
                        "REJECT: Same height, our hash {} < peer hash {} (deterministic tiebreaker)",
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    ));
                    info!(
                        "❌ Fork Resolution: REJECT peer {} - our hash wins (height {}, ours {} < peer {})",
                        params.peer_ip,
                        params.peer_height,
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    );
                    false
                }
            } else {
                reasoning.push("Same height, hashes unavailable - keeping our chain".to_string());
                warn!("⚠️  Fork Resolution: Hashes unavailable, keeping our chain");
                false
            }
        };

        ForkResolution {
            accept_peer_chain,
            reasoning,
        }
    }
}

// Compatibility types for existing code
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

impl Default for ForkResolverStats {
    fn default() -> Self {
        Self {
            total_forks: 0,
            correct_decisions: 0,
            wrong_decisions: 0,
            network_splits: 0,
            pending_outcomes: 0,
            total_peers_tracked: 0,
            avg_peer_success_rate: 0.0,
        }
    }
}

// No-op compatibility methods for backward compatibility
impl ForkResolver {
    pub async fn get_statistics(&self) -> ForkResolverStats {
        ForkResolverStats::default()
    }

    pub async fn update_fork_outcome(&self, _fork_height: u64, _outcome: ForkOutcome) {
        // No-op: we don't track outcomes in simplified version
    }

    pub async fn update_peer_reliability(
        &self,
        _peer_ip: &str,
        _was_correct: bool,
        _caused_split: bool,
    ) {
        // No-op: we don't track peer reliability in simplified version
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ForkOutcome {
    CorrectChoice,
    WrongChoice,
    NetworkSplit,
}
