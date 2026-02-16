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
    /// Cumulative stake weight supporting our chain tip
    pub our_stake_weight: u64,
    /// Stake weight of the peer (based on masternode tier)
    pub peer_stake_weight: u64,
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
            // Step 3: Same height - use stake weight first, then hash tiebreaker
            // Higher stake weight wins to resist sybil attacks (many Free-tier
            // nodes cannot outweigh a single Bronze/Silver/Gold node).
            if params.our_stake_weight != params.peer_stake_weight
                && (params.our_stake_weight > 0 || params.peer_stake_weight > 0)
            {
                if params.peer_stake_weight > params.our_stake_weight {
                    reasoning.push(format!(
                        "ACCEPT: Same height, peer stake weight {} > our weight {} (sybil-resistant tiebreaker)",
                        params.peer_stake_weight, params.our_stake_weight
                    ));
                    info!(
                        "✅ Fork Resolution: ACCEPT peer {} - higher stake weight at height {} (peer {} > ours {})",
                        params.peer_ip, params.peer_height, params.peer_stake_weight, params.our_stake_weight
                    );
                    true
                } else {
                    reasoning.push(format!(
                        "REJECT: Same height, our stake weight {} > peer weight {} (sybil-resistant tiebreaker)",
                        params.our_stake_weight, params.peer_stake_weight
                    ));
                    info!(
                        "❌ Fork Resolution: REJECT peer {} - our stake weight wins at height {} (ours {} > peer {})",
                        params.peer_ip, params.peer_height, params.our_stake_weight, params.peer_stake_weight
                    );
                    false
                }
            } else if let (Some(our_hash), Some(peer_hash)) =
                (params.our_tip_hash, params.peer_tip_hash)
            {
                // Equal stake weight or unknown — fall back to deterministic hash tiebreaker
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
