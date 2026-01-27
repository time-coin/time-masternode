//! Bounded block cache for TSDC voting
//!
//! During TSDC consensus voting, blocks need to be cached temporarily.
//! This module provides a size-bounded cache using LRU eviction to prevent
//! unbounded memory growth.
//!
//! # Memory Protection
//! - Maximum 1000 blocks cached (configurable)
//! - Automatic LRU eviction when full
//! - Optional time-based expiration
//!
//! # Usage
//! ```rust,ignore
//! let cache = BlockCache::new(1000); // Max 1000 blocks
//! cache.insert(block_hash, block);
//! if let Some(block) = cache.get(&block_hash) {
//!     // Use block
//! }
//! ```

use crate::block::types::Block;
use crate::types::Hash256;
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Entry in the block cache with metadata
#[derive(Clone)]
struct CachedBlock {
    block: Block,
    cached_at: Instant,
}

/// Thread-safe bounded block cache with LRU eviction
pub struct BlockCache {
    cache: Arc<Mutex<LruCache<Hash256, CachedBlock>>>,
    max_age: Option<Duration>,
}

impl BlockCache {
    /// Create a new block cache with specified capacity
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of blocks to cache (typically 1000)
    ///
    /// # Panics
    /// Panics if capacity is 0
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "Block cache capacity must be > 0");

        let capacity = NonZeroUsize::new(capacity).expect("Capacity must be non-zero");

        Self {
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            max_age: None,
        }
    }

    /// Create a new block cache with capacity and time-based expiration
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of blocks to cache
    /// * `max_age` - Maximum age before block is considered stale
    pub fn new_with_expiration(capacity: usize, max_age: Duration) -> Self {
        assert!(capacity > 0, "Block cache capacity must be > 0");

        let capacity = NonZeroUsize::new(capacity).expect("Capacity must be non-zero");

        Self {
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            max_age: Some(max_age),
        }
    }

    /// Insert a block into the cache
    ///
    /// If the cache is full, the least recently used block is evicted.
    /// Returns the evicted block if one was removed.
    pub fn insert(&self, hash: Hash256, block: Block) -> Option<Block> {
        let cached = CachedBlock {
            block,
            cached_at: Instant::now(),
        };

        self.cache.lock().push(hash, cached).map(|(_key, evicted)| {
            tracing::debug!("🗑️ Block cache evicted LRU block");
            evicted.block
        })
    }

    /// Get a block from the cache
    ///
    /// Returns None if:
    /// - Block not found
    /// - Block is expired (if max_age is set)
    ///
    /// Updates the LRU ordering (marks block as recently used)
    pub fn get(&self, hash: &Hash256) -> Option<Block> {
        let mut cache = self.cache.lock();

        // First check expiration without cloning the block
        if let Some(cached) = cache.peek(hash) {
            if let Some(max_age) = self.max_age {
                if cached.cached_at.elapsed() > max_age {
                    cache.pop(hash);
                    tracing::debug!("⏰ Block cache entry expired: {:?}", hash);
                    return None;
                }
            }
        }

        // If not expired, get the block (marks as recently used)
        cache.get(hash).map(|cached| cached.block.clone())
    }

    /// Check if a block exists in the cache (without updating LRU)
    pub fn contains(&self, hash: &Hash256) -> bool {
        self.cache.lock().peek(hash).is_some()
    }

    /// Remove a block from the cache
    pub fn remove(&self, hash: &Hash256) -> Option<Block> {
        self.cache.lock().pop(hash).map(|cached| cached.block)
    }

    /// Clear all blocks from the cache
    pub fn clear(&self) {
        self.cache.lock().clear();
        tracing::debug!("🧹 Block cache cleared");
    }

    /// Get the current number of blocks in the cache
    pub fn len(&self) -> usize {
        self.cache.lock().len()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.lock().is_empty()
    }

    /// Get the maximum capacity of the cache
    pub fn capacity(&self) -> usize {
        self.cache.lock().cap().get()
    }

    /// Check if any block at the given height exists in the cache
    /// Returns true if at least one block proposal exists for this height
    pub fn has_block_at_height(&self, height: u64) -> bool {
        let cache = self.cache.lock();
        cache
            .iter()
            .any(|(_, cached)| cached.block.header.height == height)
    }

    /// Remove expired entries (if max_age is set)
    ///
    /// Returns the number of expired entries removed
    pub fn cleanup_expired(&self) -> usize {
        let max_age = match self.max_age {
            Some(age) => age,
            None => return 0, // No expiration configured
        };

        let mut cache = self.cache.lock();
        let now = Instant::now();
        let mut removed = 0;

        // Collect expired keys
        let expired_keys: Vec<Hash256> = cache
            .iter()
            .filter(|(_, cached)| now.duration_since(cached.cached_at) > max_age)
            .map(|(hash, _)| *hash)
            .collect();

        // Remove expired entries
        for hash in expired_keys {
            cache.pop(&hash);
            removed += 1;
        }

        if removed > 0 {
            tracing::debug!(
                "🧹 Block cache cleanup: removed {} expired entries",
                removed
            );
        }

        removed
    }

    /// Get cache statistics
    pub fn stats(&self) -> BlockCacheStats {
        let cache = self.cache.lock();

        BlockCacheStats {
            current_size: cache.len(),
            capacity: cache.cap().get(),
            usage_percent: (cache.len() as f64 / cache.cap().get() as f64 * 100.0),
        }
    }
}

/// Statistics about the block cache
#[derive(Debug, Clone)]
pub struct BlockCacheStats {
    pub current_size: usize,
    pub capacity: usize,
    pub usage_percent: f64,
}

impl Default for BlockCache {
    fn default() -> Self {
        Self::new(1000) // Default: 1000 blocks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::types::{Block, BlockHeader};

    fn create_test_block(height: u64) -> Block {
        Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                timestamp: 0,
                block_reward: 0,
                leader: "test_leader".to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: Default::default(),
                ..Default::default()
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
            consensus_participants: vec![],
        }
    }

    #[test]
    fn test_basic_operations() {
        let cache = BlockCache::new(10);
        let block = create_test_block(1);
        let hash = [1u8; 32];

        // Insert and retrieve
        cache.insert(hash, block.clone());
        assert_eq!(cache.len(), 1);

        let retrieved = cache.get(&hash).unwrap();
        assert_eq!(retrieved.header.height, 1);

        // Contains check
        assert!(cache.contains(&hash));

        // Remove
        let removed = cache.remove(&hash).unwrap();
        assert_eq!(removed.header.height, 1);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_lru_eviction() {
        let cache = BlockCache::new(3);

        // Insert 3 blocks
        for i in 0..3 {
            let block = create_test_block(i);
            let hash = [i as u8; 32];
            cache.insert(hash, block);
        }

        assert_eq!(cache.len(), 3);

        // Insert 4th block - should evict first (LRU)
        let block4 = create_test_block(4);
        let hash4 = [4u8; 32];
        let evicted = cache.insert(hash4, block4);

        assert!(evicted.is_some());
        assert_eq!(cache.len(), 3);

        // First block should be gone
        assert!(!cache.contains(&[0u8; 32]));
        assert!(cache.contains(&[1u8; 32]));
        assert!(cache.contains(&[2u8; 32]));
        assert!(cache.contains(&hash4));
    }

    #[test]
    fn test_lru_ordering() {
        let cache = BlockCache::new(3);

        // Insert blocks 0, 1, 2
        for i in 0..3 {
            cache.insert([i as u8; 32], create_test_block(i));
        }

        // Access block 0 (makes it most recently used)
        cache.get(&[0u8; 32]);

        // Insert block 3 - should evict block 1 (now LRU)
        cache.insert([3u8; 32], create_test_block(3));

        assert!(cache.contains(&[0u8; 32])); // Still present (was accessed)
        assert!(!cache.contains(&[1u8; 32])); // Evicted (was LRU)
        assert!(cache.contains(&[2u8; 32]));
        assert!(cache.contains(&[3u8; 32]));
    }

    #[test]
    fn test_expiration() {
        let cache = BlockCache::new_with_expiration(10, Duration::from_millis(50));
        let block = create_test_block(1);
        let hash = [1u8; 32];

        cache.insert(hash, block);
        assert!(cache.contains(&hash));

        // Should still be present immediately
        assert!(cache.get(&hash).is_some());

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(60));

        // Should be expired now
        assert!(cache.get(&hash).is_none());
    }

    #[test]
    fn test_cleanup_expired() {
        let cache = BlockCache::new_with_expiration(10, Duration::from_millis(50));

        // Insert blocks
        for i in 0..5 {
            cache.insert([i as u8; 32], create_test_block(i));
        }

        assert_eq!(cache.len(), 5);

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(60));

        // Cleanup
        let removed = cache.cleanup_expired();
        assert_eq!(removed, 5);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_clear() {
        let cache = BlockCache::new(10);

        for i in 0..5 {
            cache.insert([i as u8; 32], create_test_block(i));
        }

        assert_eq!(cache.len(), 5);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn test_stats() {
        let cache = BlockCache::new(10);

        for i in 0..7 {
            cache.insert([i as u8; 32], create_test_block(i));
        }

        let stats = cache.stats();
        assert_eq!(stats.current_size, 7);
        assert_eq!(stats.capacity, 10);
        assert!((stats.usage_percent - 70.0).abs() < 0.1);
    }

    #[test]
    #[should_panic(expected = "Block cache capacity must be > 0")]
    fn test_zero_capacity_panics() {
        BlockCache::new(0);
    }
}
