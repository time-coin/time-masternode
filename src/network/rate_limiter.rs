use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    limits: HashMap<String, (Duration, u32)>,
    counters: HashMap<String, (Instant, u32)>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            limits: [
                ("tx".to_string(), (Duration::from_secs(1), 1000)),
                ("utxo_query".to_string(), (Duration::from_secs(1), 100)),
                ("subscribe".to_string(), (Duration::from_secs(60), 10)),
            ]
            .into(),
            counters: HashMap::new(),
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
        let (last_reset, count) = self.counters.entry(full_key.clone()).or_insert((now, 0));

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
