//! Centralized fork resolution state machine
//!
//! This module provides a unified approach to handling blockchain forks by
//! tracking resolution attempts, managing timeouts, and coordinating block
//! requests across multiple peers.

use crate::block::types::Block;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Maximum concurrent fork resolutions to prevent resource exhaustion
const MAX_CONCURRENT_RESOLUTIONS: usize = 5;

/// Timeout for fork resolution (60 seconds)
const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(60);

/// Phases of fork resolution process
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionPhase {
    /// Searching for common ancestor with peer's fork
    FindingCommonAncestor,

    /// Requesting blocks from peer to build alternate chain
    RequestingBlocks,

    /// Validating received chain for correctness
    ValidatingChain,

    /// Performing blockchain reorganization
    PerformingReorg,

    /// Resolution completed successfully
    Complete,

    /// Resolution failed with reason
    Failed(String),
}

/// State tracking for a single fork resolution attempt
#[derive(Debug, Clone)]
pub struct ForkResolutionState {
    /// Peer IP address we're resolving fork with
    pub peer_ip: String,

    /// Height where fork was detected
    pub fork_height: u64,

    /// Target height we're trying to reach
    pub target_height: u64,

    /// Common ancestor height (once found)
    pub common_ancestor: Option<u64>,

    /// Blocks received so far
    pub blocks_received: Vec<Block>,

    /// When this resolution started
    pub started_at: Instant,

    /// Current phase of resolution
    pub phase: ResolutionPhase,

    /// Number of block request attempts
    pub request_attempts: u32,
}

impl ForkResolutionState {
    /// Create new fork resolution state
    pub fn new(peer_ip: String, fork_height: u64, target_height: u64) -> Self {
        Self {
            peer_ip,
            fork_height,
            target_height,
            common_ancestor: None,
            blocks_received: Vec::new(),
            started_at: Instant::now(),
            phase: ResolutionPhase::FindingCommonAncestor,
            request_attempts: 0,
        }
    }

    /// Check if resolution has timed out
    pub fn is_timed_out(&self) -> bool {
        self.started_at.elapsed() > RESOLUTION_TIMEOUT
    }

    /// Check if chain is complete (continuous sequence to target)
    pub fn is_chain_complete(&self) -> bool {
        if self.blocks_received.is_empty() {
            return false;
        }

        let mut sorted = self.blocks_received.clone();
        sorted.sort_by_key(|b| b.header.height);

        // Check continuity
        let is_continuous = sorted
            .windows(2)
            .all(|w| w[1].header.height == w[0].header.height + 1);

        // Check reaches target
        let reaches_target = sorted
            .last()
            .map(|b| b.header.height >= self.target_height)
            .unwrap_or(false);

        is_continuous && reaches_target
    }

    /// Get missing block ranges that need to be requested
    pub fn get_missing_ranges(&self) -> Vec<(u64, u64)> {
        if self.blocks_received.is_empty() {
            // Need entire range
            let start = self.common_ancestor.unwrap_or(self.fork_height);
            return vec![(start, self.target_height)];
        }

        let mut sorted = self.blocks_received.clone();
        sorted.sort_by_key(|b| b.header.height);

        let mut ranges = Vec::new();
        let start = self.common_ancestor.unwrap_or(self.fork_height);

        // Find gaps in sequence
        let mut expected = start;
        for block in &sorted {
            if block.header.height > expected {
                // Gap found
                ranges.push((expected, block.header.height - 1));
            }
            expected = block.header.height + 1;
        }

        // Check if we need blocks after last received
        if expected <= self.target_height {
            ranges.push((expected, self.target_height));
        }

        ranges
    }
}

/// Centralized fork resolution manager
pub struct ForkResolver {
    /// Active fork resolutions by peer IP
    active_resolutions: HashMap<String, ForkResolutionState>,

    /// Maximum concurrent resolutions
    max_concurrent: usize,
}

impl ForkResolver {
    /// Create new fork resolver
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            active_resolutions: HashMap::new(),
            max_concurrent,
        }
    }

    /// Create with default settings
    pub fn default() -> Self {
        Self::new(MAX_CONCURRENT_RESOLUTIONS)
    }

    /// Start a new fork resolution
    /// Returns false if at capacity or resolution already exists for peer
    pub fn start_resolution(
        &mut self,
        peer_ip: String,
        fork_height: u64,
        target_height: u64,
    ) -> bool {
        // Check if already resolving with this peer
        if self.active_resolutions.contains_key(&peer_ip) {
            return false;
        }

        // Check capacity
        if self.active_resolutions.len() >= self.max_concurrent {
            return false;
        }

        let state = ForkResolutionState::new(peer_ip.clone(), fork_height, target_height);
        self.active_resolutions.insert(peer_ip, state);

        true
    }

    /// Add received blocks to an active resolution
    pub fn add_blocks(&mut self, peer_ip: &str, blocks: Vec<Block>) -> Option<ResolutionAction> {
        let state = self.active_resolutions.get_mut(peer_ip)?;

        match state.phase {
            ResolutionPhase::FindingCommonAncestor => {
                // Analyze blocks to find common ancestor
                // For now, assume first block height is common ancestor
                if let Some(first_block) = blocks.first() {
                    state.common_ancestor = Some(first_block.header.height.saturating_sub(1));
                    state.blocks_received.extend(blocks);
                    state.phase = ResolutionPhase::RequestingBlocks;

                    // Check what we still need
                    let missing = state.get_missing_ranges();
                    if missing.is_empty() {
                        state.phase = ResolutionPhase::ValidatingChain;
                        Some(ResolutionAction::ValidateChain)
                    } else {
                        Some(ResolutionAction::RequestBlocks(missing))
                    }
                } else {
                    None
                }
            }
            ResolutionPhase::RequestingBlocks => {
                state.blocks_received.extend(blocks);

                if state.is_chain_complete() {
                    state.phase = ResolutionPhase::ValidatingChain;
                    Some(ResolutionAction::ValidateChain)
                } else {
                    let missing = state.get_missing_ranges();
                    if missing.is_empty() {
                        state.phase = ResolutionPhase::ValidatingChain;
                        Some(ResolutionAction::ValidateChain)
                    } else {
                        state.request_attempts += 1;
                        Some(ResolutionAction::RequestBlocks(missing))
                    }
                }
            }
            ResolutionPhase::ValidatingChain => {
                // Already validating, ignore additional blocks
                None
            }
            _ => None,
        }
    }

    /// Mark resolution as validated and ready for reorg
    pub fn mark_validated(&mut self, peer_ip: &str) -> Option<Vec<Block>> {
        let state = self.active_resolutions.get_mut(peer_ip)?;

        if state.phase == ResolutionPhase::ValidatingChain {
            state.phase = ResolutionPhase::PerformingReorg;

            let mut sorted = state.blocks_received.clone();
            sorted.sort_by_key(|b| b.header.height);

            Some(sorted)
        } else {
            None
        }
    }

    /// Mark resolution as complete
    pub fn mark_complete(&mut self, peer_ip: &str) {
        if let Some(state) = self.active_resolutions.get_mut(peer_ip) {
            state.phase = ResolutionPhase::Complete;
        }
    }

    /// Mark resolution as failed
    pub fn mark_failed(&mut self, peer_ip: &str, reason: String) {
        if let Some(state) = self.active_resolutions.get_mut(peer_ip) {
            state.phase = ResolutionPhase::Failed(reason);
        }
    }

    /// Remove a resolution (complete or failed)
    pub fn remove_resolution(&mut self, peer_ip: &str) -> Option<ForkResolutionState> {
        self.active_resolutions.remove(peer_ip)
    }

    /// Clean up stuck/timed out resolutions
    /// Returns list of peer IPs that were stuck
    pub fn cleanup_stuck(&mut self) -> Vec<String> {
        let mut stuck = Vec::new();

        self.active_resolutions.retain(|peer_ip, state| {
            if state.is_timed_out() {
                stuck.push(peer_ip.clone());
                false // Remove
            } else {
                true // Keep
            }
        });

        stuck
    }

    /// Get current resolution state for a peer
    pub fn get_state(&self, peer_ip: &str) -> Option<&ForkResolutionState> {
        self.active_resolutions.get(peer_ip)
    }

    /// Check if currently resolving with a peer
    pub fn is_resolving(&self, peer_ip: &str) -> bool {
        self.active_resolutions.contains_key(peer_ip)
    }

    /// Get number of active resolutions
    pub fn active_count(&self) -> usize {
        self.active_resolutions.len()
    }

    /// Check if at capacity
    pub fn is_at_capacity(&self) -> bool {
        self.active_resolutions.len() >= self.max_concurrent
    }
}

/// Actions to take based on resolution state
#[derive(Debug, Clone)]
pub enum ResolutionAction {
    /// Request blocks from peer (list of ranges)
    RequestBlocks(Vec<(u64, u64)>),

    /// Validate the received chain
    ValidateChain,

    /// Perform blockchain reorganization with these blocks
    PerformReorg(Vec<Block>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::types::{Block, BlockHeader};

    fn create_test_block(height: u64) -> Block {
        Block {
            header: BlockHeader {
                version: 1,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                timestamp: 0,
                height,
                nonce: 0,
            },
            transactions: vec![],
        }
    }

    #[test]
    fn test_resolver_capacity() {
        let mut resolver = ForkResolver::new(2);

        assert!(resolver.start_resolution("peer1".to_string(), 100, 110));
        assert!(resolver.start_resolution("peer2".to_string(), 200, 210));
        assert!(!resolver.start_resolution("peer3".to_string(), 300, 310)); // At capacity

        assert_eq!(resolver.active_count(), 2);
        assert!(resolver.is_at_capacity());
    }

    #[test]
    fn test_resolution_lifecycle() {
        let mut resolver = ForkResolver::default();

        // Start resolution
        assert!(resolver.start_resolution("peer1".to_string(), 100, 105));
        assert!(resolver.is_resolving("peer1"));

        // Add blocks
        let blocks = vec![create_test_block(101), create_test_block(102)];

        let action = resolver.add_blocks("peer1", blocks);
        assert!(action.is_some());

        // Get state
        let state = resolver.get_state("peer1").unwrap();
        assert_eq!(state.phase, ResolutionPhase::RequestingBlocks);

        // Complete resolution
        resolver.mark_complete("peer1");
        let final_state = resolver.get_state("peer1").unwrap();
        assert_eq!(final_state.phase, ResolutionPhase::Complete);
    }

    #[test]
    fn test_chain_completeness() {
        let mut state = ForkResolutionState::new("peer1".to_string(), 100, 103);
        state.common_ancestor = Some(100);

        // Incomplete chain
        state.blocks_received = vec![
            create_test_block(101),
            create_test_block(103), // Gap at 102
        ];
        assert!(!state.is_chain_complete());

        // Complete chain
        state.blocks_received = vec![
            create_test_block(101),
            create_test_block(102),
            create_test_block(103),
        ];
        assert!(state.is_chain_complete());
    }

    #[test]
    fn test_missing_ranges() {
        let mut state = ForkResolutionState::new("peer1".to_string(), 100, 110);
        state.common_ancestor = Some(100);

        // No blocks received
        let ranges = state.get_missing_ranges();
        assert_eq!(ranges, vec![(100, 110)]);

        // Some blocks received with gaps
        state.blocks_received = vec![
            create_test_block(101),
            create_test_block(102),
            create_test_block(105),
            create_test_block(106),
        ];

        let ranges = state.get_missing_ranges();
        assert_eq!(ranges, vec![(103, 104), (107, 110)]);
    }

    #[test]
    fn test_cleanup_stuck() {
        let mut resolver = ForkResolver::default();

        resolver.start_resolution("peer1".to_string(), 100, 110);
        resolver.start_resolution("peer2".to_string(), 200, 210);

        // Manually set one as timed out
        if let Some(state) = resolver.active_resolutions.get_mut("peer1") {
            state.started_at = Instant::now() - Duration::from_secs(120);
        }

        let stuck = resolver.cleanup_stuck();
        assert_eq!(stuck.len(), 1);
        assert_eq!(stuck[0], "peer1");
        assert_eq!(resolver.active_count(), 1);
    }
}
