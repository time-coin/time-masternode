//! Fork Resolution System
//!
//! Simple rules, strictly enforced:
//! 1. Reject blocks with future timestamps (> 5s tolerance)
//! 2. Longer chain always wins (longest chain rule)
//! 3. Same height: deterministic hash tiebreaker (lower hash wins)

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::constants::blockchain::TIMESTAMP_TOLERANCE_SECS;

/// Fork resolution parameters
pub struct ForkResolutionParams {
    pub our_height: u64,
    pub peer_height: u64,
    pub peer_ip: String,
    pub peer_tip_timestamp: Option<i64>,
    pub our_tip_hash: Option<[u8; 32]>,
    pub peer_tip_hash: Option<[u8; 32]>,
}

/// Fork resolution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResolution {
    pub accept_peer_chain: bool,
    pub reasoning: Vec<String>,
}

/// Fork resolver — longest chain rule with deterministic hash tiebreaker
pub struct ForkResolver;

impl ForkResolver {
    pub fn new(_db: std::sync::Arc<sled::Db>) -> Self {
        Self
    }

    pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution {
        let mut reasoning = Vec::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Rule 1: Reject future timestamps
        if let Some(ts) = params.peer_tip_timestamp {
            if ts > now + TIMESTAMP_TOLERANCE_SECS {
                let delta = ts - now;
                reasoning.push(format!(
                    "REJECT: Peer tip {}s in the future (tolerance: {}s)",
                    delta, TIMESTAMP_TOLERANCE_SECS
                ));
                warn!(
                    "❌ Fork: REJECT {} — tip {}s in future",
                    params.peer_ip, delta
                );
                return ForkResolution {
                    accept_peer_chain: false,
                    reasoning,
                };
            }
        }

        // Rule 2: Longest chain wins
        if params.peer_height > params.our_height {
            let gap = params.peer_height - params.our_height;
            reasoning.push(format!(
                "ACCEPT: Peer chain is longer ({} > {}, +{} blocks)",
                params.peer_height, params.our_height, gap
            ));
            info!(
                "✅ Fork: ACCEPT {} — longer chain ({} > ours {})",
                params.peer_ip, params.peer_height, params.our_height
            );
            return ForkResolution {
                accept_peer_chain: true,
                reasoning,
            };
        }

        if params.peer_height < params.our_height {
            let gap = params.our_height - params.peer_height;
            reasoning.push(format!(
                "REJECT: Our chain is longer ({} > {}, +{} blocks)",
                params.our_height, params.peer_height, gap
            ));
            info!(
                "❌ Fork: REJECT {} — our chain is longer ({} > theirs {})",
                params.peer_ip, params.our_height, params.peer_height
            );
            return ForkResolution {
                accept_peer_chain: false,
                reasoning,
            };
        }

        // Rule 3: Same height — deterministic hash tiebreaker (lower wins)
        // This is globally consistent because all nodes see the same block hashes,
        // unlike stake weight which is subjective (each node sees different peers).
        if let (Some(our_hash), Some(peer_hash)) = (params.our_tip_hash, params.peer_tip_hash) {
            if peer_hash == our_hash {
                reasoning.push("No fork: identical chains".to_string());
                return ForkResolution {
                    accept_peer_chain: false,
                    reasoning,
                };
            }
            let accept = peer_hash < our_hash;
            reasoning.push(format!(
                "{}: Same height {}, hash tiebreaker (peer {} {} ours {})",
                if accept { "ACCEPT" } else { "REJECT" },
                params.peer_height,
                hex::encode(&peer_hash[..8]),
                if accept { "<" } else { ">" },
                hex::encode(&our_hash[..8])
            ));
            info!(
                "⚖️  Fork: {} {} — hash tiebreaker at height {}",
                if accept { "ACCEPT" } else { "REJECT" },
                params.peer_ip,
                params.peer_height
            );
            return ForkResolution {
                accept_peer_chain: accept,
                reasoning,
            };
        }

        // Fallback: keep our chain
        reasoning.push("Same height, no distinguishing data — keeping our chain".to_string());
        ForkResolution {
            accept_peer_chain: false,
            reasoning,
        }
    }
}
