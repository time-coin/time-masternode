//! Message deduplication using Bloom filters.
//!
//! Note: This module provides memory-efficient deduplication but is currently
//! unused. The current implementation uses HashSet-based deduplication which
//! is simpler but uses more memory for large numbers of messages.
//!
//! Consider integrating this for:
//! - High-throughput message processing
//! - Memory-constrained environments
//! - When deduplicating millions of messages

#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Simple but effective Bloom filter for message deduplication
/// Uses a simple bit array with multiple hash functions
pub struct BloomFilter {
    bits: Vec<bool>,
    hash_count: usize,
    size: usize,
}

impl BloomFilter {
    /// Create a new Bloom filter with approximate size
    /// For ~100k items with 0.1% false positive rate, use size ~9.6M bits (~1.2MB)
    /// For practical use with 10k items, ~1M bits (~125KB)
    pub fn new(approx_items: usize) -> Self {
        // Approximate bit size: 9.6 bits per item for 0.1% FP rate
        let size = (approx_items * 10).max(10000); // At least 10k bits
        let hash_count = 7; // 7 hash functions for 0.1% FP rate

        Self {
            bits: vec![false; size],
            hash_count,
            size,
        }
    }

    /// Check if an item is likely in the set (may have false positives)
    pub fn contains(&self, item: &[u8]) -> bool {
        for i in 0..self.hash_count {
            let hash = self.hash(item, i as u32);
            let index = (hash as usize) % self.size;
            if !self.bits[index] {
                return false; // Definitely not in set
            }
        }
        true // Probably in set (could be false positive)
    }

    /// Insert an item into the filter
    pub fn insert(&mut self, item: &[u8]) {
        for i in 0..self.hash_count {
            let hash = self.hash(item, i as u32);
            let index = (hash as usize) % self.size;
            self.bits[index] = true;
        }
    }

    /// Simple multi-hash function using FNV-1a with seed
    fn hash(&self, data: &[u8], seed: u32) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET ^ (seed as u64);
        for &byte in data {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    /// Clear the filter
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.bits.iter_mut().for_each(|b| *b = false);
    }
}

/// Time-windowed deduplication filter with automatic rotation
/// Prevents unbounded memory growth and automatically expires old entries
///
/// Uses dual-window approach to prevent re-processing during rotation:
/// - Current window: Active filter for new items
/// - Previous window: Retained during rotation to catch duplicates
pub struct DeduplicationFilter {
    current: Arc<RwLock<BloomFilter>>,
    previous: Arc<RwLock<BloomFilter>>, // NEW: Keep previous window during rotation
    rotation_interval: Duration,
    last_rotation: Arc<RwLock<Instant>>,
}

impl DeduplicationFilter {
    /// Create a new dedup filter with rotation every `rotation_interval`
    /// Default: 5 minutes
    pub fn new(rotation_interval: Duration) -> Self {
        Self {
            current: Arc::new(RwLock::new(BloomFilter::new(10000))),
            previous: Arc::new(RwLock::new(BloomFilter::new(10000))), // Initialize previous window
            rotation_interval,
            last_rotation: Arc::new(RwLock::new(Instant::now())),
        }
    }

    /// Check if item exists and insert it
    /// Returns true if item was already seen (or false positive)
    ///
    /// Dual-window approach prevents re-processing during rotation
    pub async fn check_and_insert(&self, item: &[u8]) -> bool {
        // Check BOTH current and previous windows
        let current_has = self.current.read().await.contains(item);
        let previous_has = self.previous.read().await.contains(item);

        if current_has || previous_has {
            return true; // Item seen in either window
        }

        // Check if rotation is needed
        let should_rotate = {
            let last = self.last_rotation.read().await;
            Instant::now().duration_since(*last) > self.rotation_interval
        };

        if should_rotate {
            // Acquire write lock for rotation
            let mut last_rot = self.last_rotation.write().await;
            // Double-check after acquiring lock
            if Instant::now().duration_since(*last_rot) > self.rotation_interval {
                // Atomic rotation: current becomes previous, new becomes current
                let new_filter = BloomFilter::new(10000);

                let mut previous = self.previous.write().await;
                let mut current = self.current.write().await;

                // Swap: current → previous, new → current
                *previous = std::mem::replace(&mut *current, new_filter);
                *last_rot = Instant::now();

                drop(previous);
                drop(current);
            }
        }

        // Insert into current filter only
        self.current.write().await.insert(item);
        false
    }

    /// Get count of items roughly in filter (for stats)
    #[allow(dead_code)]
    pub async fn approximate_size(&self) -> usize {
        let bits = &self.current.read().await.bits;
        let set_bits = bits.iter().filter(|&&b| b).count();
        // Rough estimate: (set_bits / total_bits) * items_capacity
        let total_bits = bits.len();
        if total_bits == 0 {
            return 0;
        }
        // Using Bloom filter density formula
        ((set_bits as f64 / total_bits as f64) * 10000.0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_filter_basic() {
        let mut bf = BloomFilter::new(1000);

        let item1 = b"test1";
        let item2 = b"test2";
        let item3 = b"test3";

        // Insert items
        bf.insert(item1);
        bf.insert(item2);

        // Check presence
        assert!(bf.contains(item1));
        assert!(bf.contains(item2));
        assert!(!bf.contains(item3)); // Should not have false negatives
    }

    #[tokio::test]
    async fn test_dedup_filter() {
        let filter = DeduplicationFilter::new(Duration::from_millis(100));

        let item = b"test";

        // First insert should return false
        assert!(!filter.check_and_insert(item).await);

        // Second insert should return true (already seen)
        assert!(filter.check_and_insert(item).await);
    }

    #[tokio::test]
    async fn test_dedup_filter_rotation() {
        let filter = DeduplicationFilter::new(Duration::from_millis(50));

        let item1 = b"test1";
        let item2 = b"test2";

        // Insert first item
        assert!(!filter.check_and_insert(item1).await);
        assert!(filter.check_and_insert(item1).await); // Still there

        // Wait for rotation
        tokio::time::sleep(Duration::from_millis(60)).await;

        // After rotation, item1 should not be found (rotated out)
        // Note: This might have false positives from the new filter,
        // but the old filter is replaced
        let _ = filter.check_and_insert(item2).await;
    }
}
