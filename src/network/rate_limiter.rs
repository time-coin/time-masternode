use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    limits: HashMap<String, (Duration, u32)>,
    counters: HashMap<String, (Instant, u32)>,
    last_cleanup: Instant,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            limits: [
                ("tx".to_string(), (Duration::from_secs(1), 1000)),
                ("utxo_query".to_string(), (Duration::from_secs(1), 100)),
                ("subscribe".to_string(), (Duration::from_secs(60), 10)),
                ("vote".to_string(), (Duration::from_secs(1), 500)),
                ("block".to_string(), (Duration::from_secs(1), 100)),
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
