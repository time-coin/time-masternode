//! Rate limiting for P2P message processing.
//!
//! Phase 2.2: DoS Protection - Message Rate Limiting
//! Implements per-peer message rate limits to prevent resource exhaustion attacks.
//!
//! # Memory Protection
//! - Hard limit of MAX_RATE_LIMIT_ENTRIES enforced
//! - Periodic cleanup every 10 seconds
//! - Emergency cleanup if approaching limit

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Maximum number of rate limit entries before forced cleanup
/// Protects against memory exhaustion during DDoS attacks
/// Each entry is ~48 bytes, so 50k entries = ~2.4MB
const MAX_RATE_LIMIT_ENTRIES: usize = 50_000;

pub struct RateLimiter {
    limits: HashMap<String, (Duration, u32)>,
    counters: HashMap<String, (Instant, u32)>,
    last_cleanup: Instant,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    /// Create new rate limiter with Phase 2.2 security limits
    pub fn new() -> Self {
        Self {
            limits: [
                // Phase 2.2: Per-peer message limits (more restrictive for DoS protection)
                ("tx".to_string(), (Duration::from_secs(1), 50)), // 50 tx/second
                ("utxo_query".to_string(), (Duration::from_secs(1), 100)), // 100 queries/sec
                ("subscribe".to_string(), (Duration::from_secs(60), 10)), // 10 subs/min
                ("vote".to_string(), (Duration::from_secs(1), 100)), // 100 votes/sec
                ("block".to_string(), (Duration::from_secs(1), 10)), // 10 blocks/sec
                ("get_blocks".to_string(), (Duration::from_secs(10), 100)), // 100 GetBlocks/10sec - generous for fork resolution
                ("get_peers".to_string(), (Duration::from_secs(60), 5)),    // 5 GetPeers/min
                (
                    "masternode_announce".to_string(),
                    (Duration::from_secs(60), 3),
                ), // 3 announcements/min - allows reconnection scenarios
                ("ping".to_string(), (Duration::from_secs(10), 2)),         // 2 pings/10sec
                ("general".to_string(), (Duration::from_secs(1), 100)),     // 100 general msgs/sec
            ]
            .into(),
            counters: HashMap::new(),
            last_cleanup: Instant::now(),
        }
    }

    pub fn check(&mut self, key: &str, ip: &str) -> bool {
        let full_key = format!("{}:{}", key, ip);
        let (window, max) = self
            .limits
            .get(key)
            .copied()
            .unwrap_or((Duration::from_secs(1), 10));

        let now = Instant::now();

        // Emergency cleanup if approaching hard limit (DDoS protection)
        if self.counters.len() >= MAX_RATE_LIMIT_ENTRIES {
            let max_age = Duration::from_secs(300); // Keep last 5 minutes
            let before_count = self.counters.len();
            self.counters
                .retain(|_, (last_reset, _)| now.duration_since(*last_reset) < max_age);
            self.last_cleanup = now;

            let removed = before_count - self.counters.len();
            if removed > 0 {
                tracing::warn!(
                    "⚠️ Rate limiter emergency cleanup: removed {} entries ({} -> {})",
                    removed,
                    before_count,
                    self.counters.len()
                );
            }
        }

        // Regular cleanup every 10 seconds
        if now.duration_since(self.last_cleanup) > Duration::from_secs(10) {
            let max_age = window * 10; // Keep entries for 10x the window duration
            self.counters
                .retain(|_, (last_reset, _)| now.duration_since(*last_reset) < max_age);
            self.last_cleanup = now;
        }

        let (last_reset, count) = self.counters.entry(full_key).or_insert((now, 0));

        if now.duration_since(*last_reset) > window {
            *last_reset = now;
            *count = 0;
        }

        if *count >= max {
            false
        } else {
            *count += 1;
            true
        }
    }

    /// Get current number of tracked rate limit entries (for monitoring)
    pub fn entry_count(&self) -> usize {
        self.counters.len()
    }

    /// Force cleanup of old entries (for testing or manual maintenance)
    pub fn force_cleanup(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.counters
            .retain(|_, (last_reset, _)| now.duration_since(*last_reset) < max_age);
        self.last_cleanup = now;
    }
}
