//! Integrated DDoS protection coordinator.
//!
//! Centralises the per-/24 subnet connection-rate tracking and provides a
//! `DDoSStats` snapshot used by the periodic health log.  The existing
//! banlist, rate limiter, connection manager, and AI attack detector remain
//! the enforcement mechanisms — this module is the coordination layer that
//! ties them together.
//!
//! ## Defence layers (in order of application)
//! 1. **Banlist** — permanent + temp IP/subnet bans, persisted via sled
//! 2. **Subnet rate** — max 20 new connections/min from any /24 (this module)
//! 3. **Connection limits** — per-IP max 3, global max 125 (connection_manager.rs)
//! 4. **Token-bucket gate** — 200 msg/s per peer, hard-kick after 300 drops (server.rs)
//! 5. **Per-message rate limits** — typed caps e.g. 50 tx/s, 10 block/s (rate_limiter.rs)
//! 6. **Handshake timeout** — 10 s to send first frame, violation recorded (server.rs)
//! 7. **AI pattern detection** — 20+ attack types, cross-peer correlation (attack_detector.rs)

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;

/// Per-/24 subnet inbound connection rate cap (non-whitelisted peers only).
/// Caps any single /24 at this many new connections per minute,
/// preventing distributed SNI floods and botnet cycling attacks (AV50).
pub const MAX_SUBNET_CONNECTS_PER_MIN: usize = 20;

/// Lightweight DDoS coordination struct.
///
/// Owns the per-/24 subnet rate-tracking table consumed by the accept loop.
/// Thin shim — actual ban/kick enforcement stays in the banlist and
/// connection manager; this struct just provides the shared state and helpers.
pub struct DDoSGuard {
    /// Per-/24 subnet inbound connection timestamps.
    /// Key: "A.B.C" (first three octets).  Value: ring of accept `Instant`s.
    pub subnet_rates: Arc<DashMap<String, VecDeque<Instant>>>,
}

impl DDoSGuard {
    pub fn new() -> Self {
        Self {
            subnet_rates: Arc::new(DashMap::new()),
        }
    }

    /// Check whether the /24 containing `ip` has exceeded the per-minute rate.
    ///
    /// Updates the sliding window as a side effect.  Returns `true` if the
    /// limit is exceeded (the caller should reject the connection).
    /// IPv6 addresses always return `false` — no /24 equivalent is defined.
    pub fn check_and_record_subnet_rate(&self, ip: IpAddr) -> bool {
        let ip_str = ip.to_string();
        if ip_str.contains(':') {
            return false;
        }
        let parts: Vec<&str> = ip_str.splitn(4, '.').collect();
        if parts.len() < 3 {
            return false;
        }
        let subnet = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
        let now = Instant::now();
        let mut entry = self.subnet_rates.entry(subnet).or_default();
        while entry
            .front()
            .map(|t: &Instant| now.duration_since(*t).as_secs() >= 60)
            .unwrap_or(false)
        {
            entry.pop_front();
        }
        entry.push_back(now);
        entry.len() > MAX_SUBNET_CONNECTS_PER_MIN
    }

    /// Prune subnet rate buckets whose youngest entry is older than 60 s.
    /// Call periodically (e.g., every 5 minutes) to prevent unbounded growth.
    pub fn cleanup_subnet_rates(&self) {
        let now = Instant::now();
        self.subnet_rates.retain(|_, v| {
            v.retain(|t| now.duration_since(*t).as_secs() < 60);
            !v.is_empty()
        });
    }
}

impl Default for DDoSGuard {
    fn default() -> Self {
        Self::new()
    }
}
