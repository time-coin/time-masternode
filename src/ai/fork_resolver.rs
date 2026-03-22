//! Fork Resolution System
//!
//! Centralizes all fork resolution logic:
//! 1. Fork decision rules (longest chain, hash tiebreaker, timestamp rejection)
//! 2. Chain validation for reorg candidates
//! 3. Common ancestor search between competing chains
//! 4. Fork resolution state machine

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::block::types::{calculate_merkle_root, calculate_merkle_root_legacy, Block};
use crate::constants::blockchain::{MAX_BLOCK_SIZE, MAX_REORG_DEPTH, TIMESTAMP_TOLERANCE_SECS};

// ===== Fork Resolution State Machine =====

/// Fork resolution state machine — tracks progress through multi-step fork resolution.
/// Moved here from blockchain.rs as it is core fork resolution logic.
#[derive(Debug, Clone)]
pub enum ForkResolutionState {
    /// No fork detected
    None,

    /// Common ancestor found, need to get peer's chain
    FetchingChain {
        common_ancestor: u64,
        fork_height: u64,
        peer_addr: String,
        peer_height: u64,
        fetched_up_to: u64,
        accumulated_blocks: Vec<Block>,
        started_at: std::time::Instant,
    },

    /// Have complete alternate chain, ready to reorg
    ReadyToReorg {
        common_ancestor: u64,
        alternate_blocks: Vec<Block>,
        started_at: std::time::Instant,
    },

    /// Performing reorganization
    Reorging {
        from_height: u64,
        to_height: u64,
        started_at: std::time::Instant,
    },
}

// ===== Fork Resolution Decision Engine =====

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

// ===== Chain Validation =====

/// Validate a sequence of blocks for internal consistency before applying a reorg.
/// This is pure validation — no I/O or state mutation.
///
/// Checks:
/// - Block heights are sequential starting from `common_ancestor + 1`
/// - Timestamps are not in the future
/// - Previous hash chain is internally consistent
/// - First block builds on `common_ancestor_hash` (if provided)
/// - Merkle roots are correct
/// - Block sizes are within limits
pub fn validate_fork_chain(
    common_ancestor: u64,
    common_ancestor_hash: Option<[u8; 32]>,
    blocks: &[Block],
    now: i64,
) -> Result<(), String> {
    if blocks.is_empty() {
        return Err("No blocks provided for fork chain validation".to_string());
    }

    // Verify first block builds on common ancestor
    if let Some(ancestor_hash) = common_ancestor_hash {
        if let Some(first_block) = blocks.first() {
            if first_block.header.previous_hash != ancestor_hash {
                return Err(format!(
                    "Fork validation failed: first block {} doesn't build on common ancestor {} \
                    (expected prev_hash {}, got {})",
                    first_block.header.height,
                    common_ancestor,
                    hex::encode(&ancestor_hash[..8]),
                    hex::encode(&first_block.header.previous_hash[..8])
                ));
            }
        }
    }

    let mut expected_prev_hash = common_ancestor_hash;

    for (index, block) in blocks.iter().enumerate() {
        let expected_height = common_ancestor + 1 + (index as u64);

        // Validate block height is sequential
        if block.header.height != expected_height {
            return Err(format!(
                "Block height mismatch: expected {}, got {}",
                expected_height, block.header.height
            ));
        }

        // Validate block timestamps are not in the future
        if block.header.timestamp > now + TIMESTAMP_TOLERANCE_SECS {
            return Err(format!(
                "Block {} timestamp {} is too far in future (now: {}, tolerance: {}s)",
                block.header.height, block.header.timestamp, now, TIMESTAMP_TOLERANCE_SECS
            ));
        }

        // Validate previous hash chain continuity
        if let Some(prev_hash) = expected_prev_hash {
            if block.header.previous_hash != prev_hash {
                return Err(format!(
                    "Chain not internally consistent: block {} previous_hash mismatch \
                    (expected {}, got {})",
                    block.header.height,
                    hex::encode(&prev_hash[..8]),
                    hex::encode(&block.header.previous_hash[..8])
                ));
            }
        }

        // Validate merkle root (try current formula, then legacy pre-txid-fix formula)
        let computed_merkle = calculate_merkle_root(&block.transactions);
        if computed_merkle != block.header.merkle_root {
            let legacy_merkle = calculate_merkle_root_legacy(&block.transactions);
            if legacy_merkle != block.header.merkle_root {
                return Err(format!(
                    "Block {} merkle root mismatch",
                    block.header.height
                ));
            }
        }

        // Validate block size
        let serialized = bincode::serialize(block).map_err(|e| e.to_string())?;
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!(
                "Block {} exceeds max size: {} > {} bytes",
                block.header.height,
                serialized.len(),
                MAX_BLOCK_SIZE
            ));
        }

        expected_prev_hash = Some(block.hash());
    }

    Ok(())
}

/// Check that a proposed reorg does not exceed the maximum allowed depth.
/// Returns `Err` with a descriptive message if it does.
pub fn check_reorg_depth(
    fork_depth: u64,
    our_height: u64,
    common_ancestor: u64,
    peer_height: u64,
    peer_addr: &str,
) -> Result<(), String> {
    if fork_depth > MAX_REORG_DEPTH {
        Err(format!(
            "REJECTED DEEP REORG from peer {} — fork depth {} exceeds maximum {} blocks \
            (our height: {}, common ancestor: {}, peer height: {}). \
            Blocks at depth >{} are considered FINAL.",
            peer_addr,
            fork_depth,
            MAX_REORG_DEPTH,
            our_height,
            common_ancestor,
            peer_height,
            MAX_REORG_DEPTH
        ))
    } else {
        Ok(())
    }
}

// ===== Common Ancestor Search =====

/// Find the common ancestor between our chain and a set of competing blocks.
///
/// Uses a linear search downward from our chain height, comparing block hashes.
/// The `get_our_block_hash` closure provides access to our chain's block hashes
/// without requiring direct access to the Blockchain struct.
///
/// Returns the height of the common ancestor, or an error if insufficient block
/// history was provided by the peer.
pub fn find_common_ancestor(
    our_height: u64,
    competing_blocks: &[Block],
    get_our_block_hash: &dyn Fn(u64) -> Result<[u8; 32], String>,
) -> Result<u64, String> {
    if competing_blocks.is_empty() {
        return Ok(0);
    }

    // Sort blocks by height
    let mut sorted_blocks = competing_blocks.to_vec();
    sorted_blocks.sort_by_key(|b| b.header.height);

    // Build a map of peer's blocks for fast lookup
    let peer_blocks: HashMap<u64, [u8; 32]> = sorted_blocks
        .iter()
        .map(|b| (b.header.height, b.hash()))
        .collect();

    let peer_height = sorted_blocks.last().unwrap().header.height;
    let peer_lowest = sorted_blocks.first().unwrap().header.height;

    info!(
        "🔍 Finding common ancestor (our: {}, peer: {}, peer blocks: {}-{})",
        our_height, peer_height, peer_lowest, peer_height
    );

    let mut candidate_ancestor = 0u64;

    // Search from our height downward
    for height in (0..=our_height).rev() {
        let our_hash = match get_our_block_hash(height) {
            Ok(hash) => hash,
            Err(_) => continue,
        };

        if let Some(peer_hash) = peer_blocks.get(&height) {
            if our_hash == *peer_hash {
                candidate_ancestor = height;
                info!(
                    "✅ Found matching block at height {} (hash {})",
                    height,
                    hex::encode(&our_hash[..8]),
                );
                break;
            } else {
                info!(
                    "🔀 Different blocks at height {}: ours {} vs peer {}",
                    height,
                    hex::encode(&our_hash[..8]),
                    hex::encode(&peer_hash[..8])
                );
                continue;
            }
        } else {
            // Peer doesn't have this block — check if peer's next block builds on ours
            if let Some(peer_next_block) =
                sorted_blocks.iter().find(|b| b.header.height == height + 1)
            {
                if peer_next_block.header.previous_hash == our_hash {
                    candidate_ancestor = height;
                    info!(
                        "✅ Found common ancestor at height {} (peer's block {} builds on ours)",
                        height,
                        height + 1
                    );
                    break;
                }
            }
        }
    }

    // Sanity check
    if candidate_ancestor > our_height {
        warn!(
            "🚫 BUG: Common ancestor {} > our height {}. Capping.",
            candidate_ancestor, our_height
        );
        candidate_ancestor = our_height;
    }

    info!(
        "🔍 Common ancestor search complete: height {} (peer lowest: {}, our: {})",
        candidate_ancestor, peer_lowest, our_height
    );

    // Validate that peer's next block actually builds on this ancestor
    if candidate_ancestor < peer_height {
        let our_block_hash = get_our_block_hash(candidate_ancestor)?;

        if let Some(peer_next_block) = sorted_blocks
            .iter()
            .find(|b| b.header.height == candidate_ancestor + 1)
        {
            let peer_next_prev_hash = peer_next_block.header.previous_hash;

            if our_block_hash != peer_next_prev_hash {
                warn!(
                    "⚠️  Validation failed: candidate ancestor {} has hash {}, \
                    but peer's block {} expects previous_hash {}",
                    candidate_ancestor,
                    hex::encode(our_block_hash),
                    candidate_ancestor + 1,
                    hex::encode(peer_next_prev_hash)
                );

                // Search backwards for the true common ancestor
                let mut true_ancestor = candidate_ancestor;
                while true_ancestor > 0 {
                    true_ancestor -= 1;

                    if let Ok(our_hash_at) = get_our_block_hash(true_ancestor) {
                        if let Some(peer_hash_at) = peer_blocks.get(&true_ancestor) {
                            if our_hash_at == *peer_hash_at {
                                if let Some(peer_next) = sorted_blocks
                                    .iter()
                                    .find(|b| b.header.height == true_ancestor + 1)
                                {
                                    if get_our_block_hash(true_ancestor).ok()
                                        == Some(peer_next.header.previous_hash)
                                    {
                                        info!(
                                            "✓ True common ancestor at height {} (corrected from {})",
                                            true_ancestor, candidate_ancestor
                                        );
                                        return Ok(true_ancestor);
                                    }
                                } else {
                                    info!(
                                        "✓ Common ancestor at height {} (no next block to validate)",
                                        true_ancestor
                                    );
                                    return Ok(true_ancestor);
                                }
                            }
                        }
                    }
                }

                if peer_lowest > 100 {
                    return Err(format!(
                        "Fork earlier than provided blocks: peer blocks start at {}, \
                        but common ancestor not found. Need deeper block history.",
                        peer_lowest
                    ));
                }

                return Ok(0);
            }
        }
    }

    // If ancestor is 0 but peer's blocks don't go back far enough
    if candidate_ancestor == 0 && peer_lowest > 100 {
        return Err(format!(
            "Insufficient block history: peer blocks only go back to height {}, \
            but common ancestor was not found (fork likely between 0 and {}).",
            peer_lowest, peer_lowest
        ));
    }

    info!("✓ Found common ancestor at height {}", candidate_ancestor);
    Ok(candidate_ancestor)
}
