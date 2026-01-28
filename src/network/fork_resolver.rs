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

        // Collect and sort heights once (avoid cloning entire blocks)
        let mut heights: Vec<u64> = self
            .blocks_received
            .iter()
            .map(|b| b.header.height)
            .collect();
        heights.sort_unstable();

        // Check continuity
        let is_continuous = heights.windows(2).all(|w| w[1] == w[0] + 1);

        // Check reaches target
        let reaches_target = heights
            .last()
            .map(|h| *h >= self.target_height)
            .unwrap_or(false);

        is_continuous && reaches_target
    }

    /// Get missing block ranges that need to be requested
    pub fn get_missing_ranges(&self) -> Vec<(u64, u64)> {
        let start = self.common_ancestor.unwrap_or(self.fork_height);

        if self.blocks_received.is_empty() {
            // Need entire range including common ancestor for validation
            return vec![(start, self.target_height)];
        }

        // Collect and sort heights once (avoid cloning entire blocks)
        let mut heights: Vec<u64> = self
            .blocks_received
            .iter()
            .map(|b| b.header.height)
            .collect();
        heights.sort_unstable();

        let mut ranges = Vec::new();

        // Find gaps in sequence
        // If we have blocks, check if we need the common ancestor block
        let first_block_height = heights[0];
        let mut expected = if self.common_ancestor.is_some() && first_block_height <= start + 1 {
            // If first received block is at common_ancestor+1 or lower, we've validated the ancestor
            first_block_height.max(start + 1)
        } else {
            // Otherwise start from common_ancestor/fork_height
            start
        };

        for &height in &heights {
            if height > expected {
                // Gap found
                ranges.push((expected, height - 1));
            }
            expected = height + 1;
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
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        Self::new(MAX_CONCURRENT_RESOLUTIONS)
    }

    /// Find common ancestor height using exponential search + binary search
    ///
    /// This is a two-phase algorithm:
    /// 1. **Exponential Search**: Jump backwards exponentially (1, 2, 4, 8, 16...)
    ///    to find a range where the common ancestor exists
    /// 2. **Binary Search**: Search within that range to find exact ancestor
    ///
    /// # Performance
    /// - 1000-block fork: ~20 requests (vs 1000 with linear search)
    /// - 10000-block fork: ~30 requests (vs 10000 with linear search)
    ///
    /// # Arguments
    /// * `our_height` - Our current blockchain height
    /// * `peer_height` - Peer's reported blockchain height  
    /// * `check_fn` - Function to check if peer has same block hash at height
    ///
    /// # Returns
    /// Height of common ancestor, or 0 if no common ancestor found
    pub async fn find_common_ancestor<F, Fut>(
        &self,
        our_height: u64,
        peer_height: u64,
        mut check_fn: F,
    ) -> Result<u64, String>
    where
        F: FnMut(u64) -> Fut,
        Fut: std::future::Future<Output = Result<bool, String>>,
    {
        // Start from the lower of the two heights
        let search_start = our_height.min(peer_height);

        tracing::debug!(
            "🔍 Starting exponential search from height {} (our: {}, peer: {})",
            search_start,
            our_height,
            peer_height
        );

        // Phase 1: Exponential backward search to find a matching height
        let mut step = 1u64;
        let lower_bound; // Will be set when we find a match

        // Check if the starting point matches
        match check_fn(search_start).await {
            Ok(true) => {
                // Chains match at this height - this is our answer
                tracing::info!("✓ Chains match at height {}", search_start);
                return Ok(search_start);
            }
            Ok(false) => {
                // Different at start - need to search backwards
                tracing::debug!("  ✗ Mismatch at starting height {}", search_start);
            }
            Err(e) => {
                return Err(format!("Error checking height {}: {}", search_start, e));
            }
        }

        // Search backwards exponentially
        loop {
            let check_height = search_start.saturating_sub(step);

            if check_height == 0 {
                // Reached genesis - check it
                match check_fn(0).await {
                    Ok(true) => {
                        lower_bound = 0;
                        break;
                    }
                    Ok(false) => {
                        // Completely incompatible chains
                        return Ok(0);
                    }
                    Err(e) => {
                        return Err(format!("Error checking genesis: {}", e));
                    }
                }
            }

            tracing::debug!("  Checking height {} (step: {})", check_height, step);

            match check_fn(check_height).await {
                Ok(true) => {
                    // Found a matching height - ancestor is between check_height and search_start
                    lower_bound = check_height;
                    tracing::debug!("  ✓ Match at height {}", check_height);
                    break;
                }
                Ok(false) => {
                    // Still different - continue searching backwards
                    tracing::debug!("  ✗ Mismatch at height {}", check_height);
                }
                Err(e) => {
                    return Err(format!("Error checking height {}: {}", check_height, e));
                }
            }

            // Double the step for next iteration
            step = step.saturating_mul(2);

            // Safety limit: if step gets too large, cap it
            if step > 100_000 {
                step = 100_000;
            }
        }

        // Phase 2: Binary search between lower_bound and search_start
        let mut low = lower_bound;
        let mut high = search_start;

        tracing::debug!(
            "🔍 Binary search between {} and {} (range: {})",
            low,
            high,
            high - low
        );

        while low < high {
            let mid = low + (high - low) / 2;

            tracing::debug!("  Checking mid-point {}", mid);

            match check_fn(mid).await {
                Ok(true) => {
                    // Match at mid - ancestor might be higher
                    low = mid + 1;
                    tracing::debug!("  ✓ Match at {}, search upper half", mid);
                }
                Ok(false) => {
                    // Mismatch at mid - ancestor is below mid
                    high = mid;
                    tracing::debug!("  ✗ Mismatch at {}, search lower half", mid);
                }
                Err(e) => {
                    return Err(format!("Error checking height {}: {}", mid, e));
                }
            }
        }

        // The ancestor is at low - 1 (last matching height)
        let ancestor = if low > 0 { low - 1 } else { 0 };

        tracing::info!("✓ Found common ancestor at height {}", ancestor);

        Ok(ancestor)
    }

    /// Find common ancestor with request counting (for testing/metrics)
    ///
    /// Same as `find_common_ancestor` but tracks number of requests made
    #[allow(dead_code)]
    pub async fn find_common_ancestor_with_metrics<F, Fut>(
        &self,
        our_height: u64,
        peer_height: u64,
        mut check_fn: F,
    ) -> Result<(u64, usize), String>
    where
        F: FnMut(u64) -> Fut,
        Fut: std::future::Future<Output = Result<bool, String>>,
    {
        let mut request_count = 0;
        let search_start = our_height.min(peer_height);

        // Wrapper to count requests
        let mut counted_check = |height: u64| {
            request_count += 1;
            check_fn(height)
        };

        // Phase 1: Check starting point
        match counted_check(search_start).await {
            Ok(true) => return Ok((search_start, request_count)),
            Ok(false) => {}
            Err(e) => return Err(format!("Error checking height {}: {}", search_start, e)),
        }

        // Phase 1: Exponential search
        let mut step = 1u64;
        let lower_bound; // Will be set when we find a match

        loop {
            let check_height = search_start.saturating_sub(step);

            if check_height == 0 {
                match counted_check(0).await {
                    Ok(true) => {
                        lower_bound = 0;
                        break;
                    }
                    Ok(false) => return Ok((0, request_count)),
                    Err(e) => return Err(format!("Error checking genesis: {}", e)),
                }
            }

            match counted_check(check_height).await {
                Ok(true) => {
                    lower_bound = check_height;
                    break;
                }
                Ok(false) => {}
                Err(e) => return Err(format!("Error checking height {}: {}", check_height, e)),
            }

            step = step.saturating_mul(2);
            if step > 100_000 {
                step = 100_000;
            }
        }

        // Phase 2: Binary search
        let mut low = lower_bound;
        let mut high = search_start;

        while low < high {
            let mid = low + (high - low) / 2;

            match counted_check(mid).await {
                Ok(true) => low = mid + 1,
                Ok(false) => high = mid,
                Err(e) => return Err(format!("Error checking height {}: {}", mid, e)),
            }
        }

        let ancestor = if low > 0 { low - 1 } else { 0 };
        Ok((ancestor, request_count))
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
        use crate::block::types::MasternodeTierCounts;

        Block {
            header: BlockHeader {
                version: 1,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                timestamp: 0,
                height,
                block_reward: 0,
                leader: String::new(),
                attestation_root: [0u8; 32],
                masternode_tiers: MasternodeTierCounts {
                    free: 0,
                    bronze: 0,
                    silver: 0,
                    gold: 0,
                },
                ..Default::default()
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
            consensus_participants: vec![],
            liveness_recovery: Some(false),
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

    // Tests for exponential search algorithm

    #[tokio::test]
    async fn test_find_ancestor_exact_match() {
        let resolver = ForkResolver::default();

        // Simulate peer with identical chain
        let check_fn = |_height: u64| async move { Ok(true) };

        let ancestor = resolver.find_common_ancestor(1000, 1000, check_fn).await;
        assert_eq!(ancestor.unwrap(), 1000);
    }

    #[tokio::test]
    async fn test_find_ancestor_deep_fork() {
        let resolver = ForkResolver::default();

        // Simulate fork at height 100 (everything below matches, above differs)
        let fork_point = 100u64;
        let check_fn = |height: u64| async move { Ok(height <= fork_point) };

        let (ancestor, requests) = resolver
            .find_common_ancestor_with_metrics(1000, 1000, check_fn)
            .await
            .unwrap();

        assert_eq!(ancestor, fork_point);

        // With 900-block fork, exponential + binary should use ~20-30 requests
        // vs 900 with linear search
        assert!(
            requests < 35,
            "Expected <35 requests for 900-block fork, got {}",
            requests
        );
        println!("✓ Deep fork (900 blocks): {} requests", requests);
    }

    #[tokio::test]
    async fn test_find_ancestor_recent_fork() {
        let resolver = ForkResolver::default();

        // Simulate recent fork (last 10 blocks differ)
        let fork_point = 990u64;
        let check_fn = |height: u64| async move { Ok(height <= fork_point) };

        let (ancestor, requests) = resolver
            .find_common_ancestor_with_metrics(1000, 1000, check_fn)
            .await
            .unwrap();

        assert_eq!(ancestor, fork_point);

        // Recent fork should be found very quickly
        assert!(
            requests < 15,
            "Expected <15 requests for recent fork, got {}",
            requests
        );
        println!("✓ Recent fork (10 blocks): {} requests", requests);
    }

    #[tokio::test]
    async fn test_find_ancestor_genesis_fork() {
        let resolver = ForkResolver::default();

        // Completely incompatible chains (no common ancestor except genesis)
        let check_fn = |height: u64| async move { Ok(height == 0) };

        let (ancestor, requests) = resolver
            .find_common_ancestor_with_metrics(1000, 1000, check_fn)
            .await
            .unwrap();

        assert_eq!(ancestor, 0);

        // Should still be efficient
        assert!(
            requests < 25,
            "Expected <25 requests for genesis fork, got {}",
            requests
        );
        println!("✓ Genesis fork: {} requests", requests);
    }

    #[tokio::test]
    async fn test_find_ancestor_different_heights() {
        let resolver = ForkResolver::default();

        // Our chain: 500 blocks, peer chain: 1000 blocks
        // Fork at height 400
        let fork_point = 400u64;
        let check_fn = |height: u64| async move { Ok(height <= fork_point) };

        let (ancestor, requests) = resolver
            .find_common_ancestor_with_metrics(500, 1000, check_fn)
            .await
            .unwrap();

        assert_eq!(ancestor, fork_point);

        // Should search from min(500, 1000) = 500
        assert!(requests < 20, "Expected <20 requests, got {}", requests);
        println!("✓ Different heights (500 vs 1000): {} requests", requests);
    }

    #[tokio::test]
    async fn test_find_ancestor_very_deep_fork() {
        let resolver = ForkResolver::default();

        // Simulate extremely deep fork (10,000 blocks)
        let fork_point = 100u64;
        let check_fn = |height: u64| async move { Ok(height <= fork_point) };

        let (ancestor, requests) = resolver
            .find_common_ancestor_with_metrics(10_100, 10_100, check_fn)
            .await
            .unwrap();

        assert_eq!(ancestor, fork_point);

        // Even with 10,000 block fork, should stay under 40 requests
        // Exponential search: ~14 steps to get from 10000 to <100 (2^14 = 16384)
        // Binary search: ~7 steps within final range
        // Total: ~21 requests expected
        assert!(
            requests < 40,
            "Expected <40 requests for 10,000-block fork, got {}",
            requests
        );
        println!("✓ Very deep fork (10,000 blocks): {} requests", requests);
    }

    #[tokio::test]
    async fn test_find_ancestor_peer_ahead() {
        let resolver = ForkResolver::default();

        // Peer is ahead of us, fork at height 50
        let fork_point = 50u64;
        let check_fn = |height: u64| async move { Ok(height <= fork_point) };

        let (ancestor, requests) = resolver
            .find_common_ancestor_with_metrics(100, 500, check_fn)
            .await
            .unwrap();

        assert_eq!(ancestor, fork_point);

        // Should search from min(100, 500) = 100
        assert!(requests < 15, "Expected <15 requests, got {}", requests);
        println!("✓ Peer ahead (100 vs 500): {} requests", requests);
    }

    #[tokio::test]
    async fn test_exponential_search_efficiency() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let resolver = ForkResolver::default();

        // Test various fork depths and measure efficiency
        let test_cases = vec![
            (50, "50-block fork"),
            (100, "100-block fork"),
            (1000, "1000-block fork"),
            (10000, "10000-block fork"),
        ];

        for (fork_depth, description) in test_cases {
            let counter = AtomicUsize::new(0);
            let fork_point = 100u64;

            let check_fn = |height: u64| {
                counter.fetch_add(1, Ordering::Relaxed);
                async move { Ok(height <= fork_point) }
            };

            let ancestor = resolver
                .find_common_ancestor(100 + fork_depth, 100 + fork_depth, check_fn)
                .await
                .unwrap();

            let requests = counter.load(Ordering::Relaxed);
            let efficiency = requests as f64 / fork_depth as f64 * 100.0;

            assert_eq!(ancestor, fork_point);

            println!(
                "✓ {}: {} requests ({:.2}% of linear search)",
                description, requests, efficiency
            );

            // Verify exponential is much better than linear
            // Should use < 20% of linear search requests for larger forks
            let max_expected = if fork_depth < 100 {
                fork_depth // Small forks might not benefit as much
            } else {
                fork_depth / 5 // Larger forks should use < 20%
            };

            assert!(
                requests <= max_expected as usize,
                "{}: used {} requests (expected <= {})",
                description,
                requests,
                max_expected
            );
        }
    }
}
