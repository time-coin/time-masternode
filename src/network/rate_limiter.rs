//! Rate limiting for P2P message processing.
//!
//! Phase 2.2: DoS Protection - Message Rate Limiting
//! Implements per-peer message rate limits to prevent resource exhaustion attacks.

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    limits: HashMap<String, (Duration, u32)>,
    counters: HashMap<String, (Instant, u32)>,
    last_cleanup: Instant,
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
                ("get_blocks".to_string(), (Duration::from_secs(10), 5)), // 5 GetBlocks/10sec
                ("get_peers".to_string(), (Duration::from_secs(60), 5)), // 5 GetPeers/min
                (
                    "masternode_announce".to_string(),
                    (Duration::from_secs(300), 1),
                ), // 1 announcement/5min
                ("ping".to_string(), (Duration::from_secs(10), 2)), // 2 pings/10sec
                ("general".to_string(), (Duration::from_secs(1), 100)), // 100 general msgs/sec
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

        // Cleanup expired entries every 10 seconds (prevents unbounded memory growth)
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
}
