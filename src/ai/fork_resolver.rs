//! Stake-Weighted Fork Resolution System
//!
//! Uses longest valid chain as the primary rule, but allows stake weight
//! to override small height differences (≤ MAX_STAKE_OVERRIDE_DEPTH blocks).
//! This prevents low-stake sybil chains from outrunning high-stake chains
//! during brief network partitions.
//!
//! Rules:
//! - If height gap > MAX_STAKE_OVERRIDE_DEPTH → longest chain wins (pure height)
//! - If height gap ≤ MAX_STAKE_OVERRIDE_DEPTH → shorter chain wins if it has
//!   ≥ MIN_STAKE_OVERRIDE_RATIO × the taller chain's cumulative stake
//! - If same height → stake weight tiebreaker, then hash tiebreaker
//! - Validate timestamps are not in future (with tolerance)

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

// Timestamp tolerance for network conditions (60 seconds)
// Accounts for: network latency, clock drift, processing delays
const TIMESTAMP_TOLERANCE_SECS: i64 = 60;

/// Maximum height deficit (in blocks) that stake weight can override.
/// Beyond this depth, the longer chain always wins regardless of stake.
/// With 600s block time, 2 blocks ≈ 20 minutes — a reasonable partition window.
pub const MAX_STAKE_OVERRIDE_DEPTH: u64 = 2;

/// Minimum stake ratio required to override a height advantage.
/// The shorter chain must have at least this multiple of the taller chain's
/// cumulative stake weight to be preferred.
const MIN_STAKE_OVERRIDE_RATIO: u64 = 2;

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

/// Fork resolution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResolution {
    pub accept_peer_chain: bool,
    /// True when the decision was based on stake weight overriding a height advantage
    pub stake_override: bool,
    pub reasoning: Vec<String>,
}

/// Fork resolver with stake-weighted chain selection
pub struct ForkResolver;

impl ForkResolver {
    pub fn new(_db: std::sync::Arc<sled::Db>) -> Self {
        Self
    }

    /// Fork resolution: longest chain wins, but stake weight can override small height gaps
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
                    stake_override: false,
                    reasoning,
                };
            }
        }

        // Step 2: Height + stake comparison
        let height_diff = params.peer_height.abs_diff(params.our_height);

        let (accept_peer_chain, stake_override) = if height_diff == 0 {
            // Same height — stake weight tiebreaker, then hash
            self.resolve_same_height(&params, &mut reasoning)
        } else if height_diff <= MAX_STAKE_OVERRIDE_DEPTH {
            // Small height gap — stake weight can override
            self.resolve_small_gap(&params, height_diff, &mut reasoning)
        } else {
            // Large gap — pure longest chain rule
            self.resolve_large_gap(&params, height_diff, &mut reasoning)
        };

        ForkResolution {
            accept_peer_chain,
            stake_override,
            reasoning,
        }
    }

    /// Same height: stake weight first, then deterministic hash tiebreaker
    fn resolve_same_height(
        &self,
        params: &ForkResolutionParams,
        reasoning: &mut Vec<String>,
    ) -> (bool, bool) {
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
                (true, false)
            } else {
                reasoning.push(format!(
                    "REJECT: Same height, our stake weight {} > peer weight {} (sybil-resistant tiebreaker)",
                    params.our_stake_weight, params.peer_stake_weight
                ));
                info!(
                    "❌ Fork Resolution: REJECT peer {} - our stake weight wins at height {} (ours {} > peer {})",
                    params.peer_ip, params.peer_height, params.our_stake_weight, params.peer_stake_weight
                );
                (false, false)
            }
        } else if let (Some(our_hash), Some(peer_hash)) =
            (params.our_tip_hash, params.peer_tip_hash)
        {
            if peer_hash == our_hash {
                reasoning.push("No fork: identical chains".to_string());
                info!("✅ No fork: peer {} has identical chain", params.peer_ip);
                (false, false)
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
                (true, false)
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
                (false, false)
            }
        } else {
            reasoning.push("Same height, hashes unavailable - keeping our chain".to_string());
            warn!("⚠️  Fork Resolution: Hashes unavailable, keeping our chain");
            (false, false)
        }
    }

    /// Small height gap (≤ MAX_STAKE_OVERRIDE_DEPTH): stake can override height
    fn resolve_small_gap(
        &self,
        params: &ForkResolutionParams,
        height_diff: u64,
        reasoning: &mut Vec<String>,
    ) -> (bool, bool) {
        let peer_is_taller = params.peer_height > params.our_height;

        // The shorter chain needs overwhelmingly more stake to override height
        let (shorter_stake, taller_stake) = if peer_is_taller {
            (params.our_stake_weight, params.peer_stake_weight)
        } else {
            (params.peer_stake_weight, params.our_stake_weight)
        };

        let required_stake = taller_stake.saturating_mul(MIN_STAKE_OVERRIDE_RATIO);

        if shorter_stake > 0 && shorter_stake >= required_stake {
            // Shorter chain has dominant stake — override height advantage
            let ratio = shorter_stake / taller_stake.max(1);
            let action = if peer_is_taller { "REJECT" } else { "ACCEPT" };
            reasoning.push(format!(
                "{}: {} block(s) behind but stake override ({} vs {}, ratio {}x ≥ {}x required)",
                action, height_diff, shorter_stake, taller_stake, ratio, MIN_STAKE_OVERRIDE_RATIO
            ));
            if peer_is_taller {
                info!(
                    "⚖️  Fork Resolution: REJECT peer {} - our stake override ({}x) at height gap {} (ours {} vs peer {})",
                    params.peer_ip, ratio, height_diff, params.our_height, params.peer_height
                );
            } else {
                info!(
                    "⚖️  Fork Resolution: ACCEPT peer {} - peer stake override ({}x) at height gap {} (peer {} vs ours {})",
                    params.peer_ip, ratio, height_diff, params.peer_height, params.our_height
                );
            }
            // accept_peer_chain = true only if WE are the taller chain (peer's shorter chain wins)
            (!peer_is_taller, true)
        } else {
            // Not enough stake advantage — longer chain wins (default)
            let action = if peer_is_taller { "ACCEPT" } else { "REJECT" };
            reasoning.push(format!(
                "{}: Peer is {} block(s) {} (shorter stake {} < {}x required = {})",
                action,
                height_diff,
                if peer_is_taller { "ahead" } else { "behind" },
                shorter_stake,
                MIN_STAKE_OVERRIDE_RATIO,
                required_stake
            ));
            if peer_is_taller {
                info!(
                    "✅ Fork Resolution: ACCEPT peer {} - longer chain wins (height {} > {}, stake ratio insufficient)",
                    params.peer_ip, params.peer_height, params.our_height
                );
            } else {
                info!(
                    "❌ Fork Resolution: REJECT peer {} - our chain is longer ({} > {}, stake ratio insufficient)",
                    params.peer_ip, params.our_height, params.peer_height
                );
            }
            (peer_is_taller, false)
        }
    }

    /// Large height gap (> MAX_STAKE_OVERRIDE_DEPTH): pure longest chain rule
    fn resolve_large_gap(
        &self,
        params: &ForkResolutionParams,
        height_diff: u64,
        reasoning: &mut Vec<String>,
    ) -> (bool, bool) {
        if params.peer_height > params.our_height {
            reasoning.push(format!(
                "ACCEPT: Peer has longer chain ({} > {}, gap {} > max stake override depth {})",
                params.peer_height, params.our_height, height_diff, MAX_STAKE_OVERRIDE_DEPTH
            ));
            info!(
                "✅ Fork Resolution: ACCEPT peer {} - longer chain (height {} > ours {}, beyond stake override range)",
                params.peer_ip, params.peer_height, params.our_height
            );
            (true, false)
        } else {
            reasoning.push(format!(
                "REJECT: Our chain is longer ({} > {}, gap {} > max stake override depth {})",
                params.our_height, params.peer_height, height_diff, MAX_STAKE_OVERRIDE_DEPTH
            ));
            info!(
                "❌ Fork Resolution: REJECT peer {} - our chain is longer (height {} > theirs {}, beyond stake override range)",
                params.peer_ip, params.our_height, params.peer_height
            );
            (false, false)
        }
    }
}
