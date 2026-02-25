//! Adaptive Reconnection AI
//!
//! Learns optimal reconnection strategies for peers based on historical success patterns.
//! Prevents aggressive reconnection to consistently failing peers while ensuring
//! reliable peers are reconnected quickly.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tracing::debug;

/// Per-peer connection history and learned parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConnectionProfile {
    pub ip: String,
    pub is_masternode: bool,

    // Connection statistics
    pub total_connections: u64,
    pub successful_connections: u64,
    pub failed_connections: u64,
    pub total_uptime_secs: u64,
    pub avg_session_duration_secs: f64,

    // Failure patterns
    pub consecutive_failures: u32,
    pub last_failure_time: u64,
    pub failure_reasons: HashMap<String, u32>,

    // Timing patterns
    pub avg_time_to_connect_ms: f64,
    pub best_time_of_day: Option<u8>, // Hour (0-23) when most successful
    pub worst_time_of_day: Option<u8>,

    // Learned parameters
    pub optimal_retry_delay_secs: f64,
    pub reliability_score: f64, // 0.0-1.0
    pub last_updated: u64,
}

impl Default for PeerConnectionProfile {
    fn default() -> Self {
        Self {
            ip: String::new(),
            is_masternode: false,
            total_connections: 0,
            successful_connections: 0,
            failed_connections: 0,
            total_uptime_secs: 0,
            avg_session_duration_secs: 0.0,
            consecutive_failures: 0,
            last_failure_time: 0,
            failure_reasons: HashMap::new(),
            avg_time_to_connect_ms: 1000.0,
            best_time_of_day: None,
            worst_time_of_day: None,
            optimal_retry_delay_secs: 5.0,
            reliability_score: 0.5,
            last_updated: now_secs(),
        }
    }
}

/// Reconnection recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectionAdvice {
    pub should_attempt: bool,
    pub delay_secs: u64,
    pub priority: ReconnectionPriority,
    pub reasoning: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReconnectionPriority {
    Critical = 4, // Masternode, must reconnect immediately
    High = 3,     // Reliable peer, reconnect soon
    Normal = 2,   // Average peer
    Low = 1,      // Unreliable peer, wait longer
    Skip = 0,     // Don't bother reconnecting
}

/// Configuration for the reconnection AI
#[derive(Debug, Clone)]
pub struct ReconnectionConfig {
    pub min_retry_delay_secs: f64,
    pub max_retry_delay_secs: f64,
    pub backoff_multiplier: f64,
    pub reliability_threshold: f64,    // Below this, reduce priority
    pub max_consecutive_failures: u32, // After this many failures, enter cooldown
    pub cooldown_period_secs: u64,
    pub learning_rate: f64,
}

impl Default for ReconnectionConfig {
    fn default() -> Self {
        Self {
            min_retry_delay_secs: 2.0,
            max_retry_delay_secs: 300.0, // 5 minutes max
            backoff_multiplier: 1.5,
            reliability_threshold: 0.3,
            max_consecutive_failures: 3,
            cooldown_period_secs: 600, // 10 minute cooldown
            learning_rate: 0.1,
        }
    }
}

/// Main Adaptive Reconnection AI
pub struct AdaptiveReconnectionAI {
    /// Per-peer profiles
    profiles: Arc<RwLock<HashMap<String, PeerConnectionProfile>>>,

    /// Configuration
    config: ReconnectionConfig,

    /// Global network health factor (affects all reconnections)
    network_health: Arc<RwLock<f64>>,
}

impl AdaptiveReconnectionAI {
    pub fn new(config: ReconnectionConfig) -> Self {
        Self {
            profiles: Arc::new(RwLock::new(HashMap::new())),
            config,
            network_health: Arc::new(RwLock::new(1.0)),
        }
    }

    /// Record a successful connection
    pub fn record_connection_success(&self, ip: &str, is_masternode: bool, connect_time_ms: u64) {
        let mut profiles = self.profiles.write();
        let profile = profiles
            .entry(ip.to_string())
            .or_insert_with(|| PeerConnectionProfile {
                ip: ip.to_string(),
                is_masternode,
                ..Default::default()
            });

        profile.total_connections += 1;
        profile.successful_connections += 1;
        profile.consecutive_failures = 0;
        profile.is_masternode = is_masternode;

        // Update timing with exponential moving average
        let alpha = self.config.learning_rate;
        profile.avg_time_to_connect_ms =
            (1.0 - alpha) * profile.avg_time_to_connect_ms + alpha * connect_time_ms as f64;

        // Update optimal retry delay (reduce since we succeeded)
        profile.optimal_retry_delay_secs =
            (profile.optimal_retry_delay_secs * 0.9).max(self.config.min_retry_delay_secs);

        // Update reliability score
        self.update_reliability_score(profile);

        // Track time of day patterns
        let hour = current_hour();
        profile.best_time_of_day = Some(hour);

        profile.last_updated = now_secs();

        debug!(
            "✅ Connection success for {}: reliability={:.2}, avg_connect={:.0}ms",
            ip, profile.reliability_score, profile.avg_time_to_connect_ms
        );
    }

    /// Record a connection failure
    pub fn record_connection_failure(&self, ip: &str, is_masternode: bool, reason: &str) {
        let mut profiles = self.profiles.write();
        let profile = profiles
            .entry(ip.to_string())
            .or_insert_with(|| PeerConnectionProfile {
                ip: ip.to_string(),
                is_masternode,
                ..Default::default()
            });

        profile.total_connections += 1;
        profile.failed_connections += 1;
        profile.consecutive_failures += 1;
        profile.last_failure_time = now_secs();
        profile.is_masternode = is_masternode;

        // Track failure reasons
        *profile
            .failure_reasons
            .entry(reason.to_string())
            .or_insert(0) += 1;

        // Update optimal retry delay (increase with backoff)
        profile.optimal_retry_delay_secs = (profile.optimal_retry_delay_secs
            * self.config.backoff_multiplier)
            .min(self.config.max_retry_delay_secs);

        // Update reliability score
        self.update_reliability_score(profile);

        // Track time of day patterns
        let hour = current_hour();
        profile.worst_time_of_day = Some(hour);

        profile.last_updated = now_secs();

        debug!(
            "❌ Connection failure for {}: consecutive={}, reliability={:.2}, next_retry={:.0}s",
            ip,
            profile.consecutive_failures,
            profile.reliability_score,
            profile.optimal_retry_delay_secs
        );
    }

    /// Record session end (disconnection)
    pub fn record_session_end(&self, ip: &str, session_duration_secs: u64) {
        let mut profiles = self.profiles.write();
        if let Some(profile) = profiles.get_mut(ip) {
            profile.total_uptime_secs += session_duration_secs;

            // Update average session duration
            let alpha = self.config.learning_rate;
            profile.avg_session_duration_secs = (1.0 - alpha) * profile.avg_session_duration_secs
                + alpha * session_duration_secs as f64;

            profile.last_updated = now_secs();
        }
    }

    /// Get reconnection advice for a peer
    pub fn get_reconnection_advice(&self, ip: &str, is_masternode: bool) -> ReconnectionAdvice {
        let profiles = self.profiles.read();
        let network_health = *self.network_health.read();

        let profile = profiles.get(ip);

        match profile {
            None => {
                // Unknown peer - use default strategy
                let delay = if is_masternode { 2 } else { 5 };
                ReconnectionAdvice {
                    should_attempt: true,
                    delay_secs: delay,
                    priority: if is_masternode {
                        ReconnectionPriority::Critical
                    } else {
                        ReconnectionPriority::Normal
                    },
                    reasoning: "Unknown peer, using default strategy".to_string(),
                }
            }
            Some(profile) => self.calculate_advice(profile, network_health),
        }
    }

    /// Get all peers sorted by reconnection priority
    pub fn get_reconnection_queue(&self) -> Vec<(String, ReconnectionAdvice)> {
        let profiles = self.profiles.read();
        let network_health = *self.network_health.read();

        let mut queue: Vec<_> = profiles
            .values()
            .map(|p| {
                let advice = self.calculate_advice(p, network_health);
                (p.ip.clone(), advice)
            })
            .filter(|(_, advice)| advice.should_attempt)
            .collect();

        // Sort by priority (highest first), then by delay (lowest first)
        queue.sort_by(|a, b| {
            b.1.priority
                .cmp(&a.1.priority)
                .then(a.1.delay_secs.cmp(&b.1.delay_secs))
        });

        queue
    }

    /// Update network health factor (called from consensus health monitor)
    pub fn set_network_health(&self, health: f64) {
        *self.network_health.write() = health.clamp(0.0, 1.0);
    }

    /// Get peer statistics
    pub fn get_peer_stats(&self, ip: &str) -> Option<PeerConnectionProfile> {
        self.profiles.read().get(ip).cloned()
    }

    /// Get overall reconnection statistics
    pub fn get_stats(&self) -> ReconnectionStats {
        let profiles = self.profiles.read();

        let total_peers = profiles.len();
        let masternode_count = profiles.values().filter(|p| p.is_masternode).count();

        let avg_reliability = if !profiles.is_empty() {
            profiles.values().map(|p| p.reliability_score).sum::<f64>() / total_peers as f64
        } else {
            0.0
        };

        let high_reliability_peers = profiles
            .values()
            .filter(|p| p.reliability_score > 0.8)
            .count();

        let peers_in_cooldown = profiles
            .values()
            .filter(|p| p.consecutive_failures >= self.config.max_consecutive_failures)
            .count();

        ReconnectionStats {
            total_peers,
            masternode_count,
            avg_reliability,
            high_reliability_peers,
            peers_in_cooldown,
        }
    }

    fn calculate_advice(
        &self,
        profile: &PeerConnectionProfile,
        network_health: f64,
    ) -> ReconnectionAdvice {
        let now = now_secs();

        // Check if in cooldown
        if profile.consecutive_failures >= self.config.max_consecutive_failures {
            let time_since_failure = now.saturating_sub(profile.last_failure_time);
            if time_since_failure < self.config.cooldown_period_secs {
                let remaining = self.config.cooldown_period_secs - time_since_failure;
                return ReconnectionAdvice {
                    should_attempt: false,
                    delay_secs: remaining,
                    priority: ReconnectionPriority::Skip,
                    reasoning: format!(
                        "In cooldown after {} consecutive failures ({} secs remaining)",
                        profile.consecutive_failures, remaining
                    ),
                };
            }
        }

        // Determine priority
        let priority = if profile.is_masternode {
            ReconnectionPriority::Critical
        } else if profile.reliability_score > 0.8 {
            ReconnectionPriority::High
        } else if profile.reliability_score > self.config.reliability_threshold {
            ReconnectionPriority::Normal
        } else {
            ReconnectionPriority::Low
        };

        // Calculate delay based on learned optimal and network health
        let base_delay = profile.optimal_retry_delay_secs;

        // Adjust for network health (faster reconnect if network is unhealthy)
        let health_factor = if network_health < 0.5 { 0.5 } else { 1.0 };

        // Adjust for masternode status (faster reconnect)
        let masternode_factor = if profile.is_masternode { 0.5 } else { 1.0 };

        // Adjust for consecutive failures
        let failure_factor = 1.0 + (profile.consecutive_failures as f64 * 0.5);

        let adjusted_delay = (base_delay * health_factor * masternode_factor * failure_factor)
            .max(self.config.min_retry_delay_secs)
            .min(self.config.max_retry_delay_secs);

        let reasoning = format!(
            "reliability={:.2}, failures={}, base_delay={:.0}s, adjusted={:.0}s",
            profile.reliability_score, profile.consecutive_failures, base_delay, adjusted_delay
        );

        ReconnectionAdvice {
            should_attempt: true,
            delay_secs: adjusted_delay as u64,
            priority,
            reasoning,
        }
    }

    fn update_reliability_score(&self, profile: &mut PeerConnectionProfile) {
        if profile.total_connections == 0 {
            profile.reliability_score = 0.5;
            return;
        }

        // Base reliability on success rate
        let success_rate = profile.successful_connections as f64 / profile.total_connections as f64;

        // Weight recent performance more heavily
        let recency_factor = if profile.consecutive_failures > 0 {
            1.0 / (1.0 + profile.consecutive_failures as f64 * 0.1)
        } else {
            1.0
        };

        // Session duration factor (longer sessions = more reliable)
        let duration_factor = if profile.avg_session_duration_secs > 600.0 {
            1.1 // Bonus for long sessions
        } else if profile.avg_session_duration_secs < 60.0 {
            0.9 // Penalty for short sessions
        } else {
            1.0
        };

        profile.reliability_score =
            (success_rate * recency_factor * duration_factor).clamp(0.0, 1.0);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconnectionStats {
    pub total_peers: usize,
    pub masternode_count: usize,
    pub avg_reliability: f64,
    pub high_reliability_peers: usize,
    pub peers_in_cooldown: usize,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn current_hour() -> u8 {
    let secs = now_secs();
    ((secs % 86400) / 3600) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_peer_advice() {
        let ai = AdaptiveReconnectionAI::new(ReconnectionConfig::default());

        let advice = ai.get_reconnection_advice("192.168.1.1", false);
        assert!(advice.should_attempt);
        assert_eq!(advice.priority, ReconnectionPriority::Normal);
    }

    #[test]
    fn test_masternode_priority() {
        let ai = AdaptiveReconnectionAI::new(ReconnectionConfig::default());

        let advice = ai.get_reconnection_advice("192.168.1.1", true);
        assert_eq!(advice.priority, ReconnectionPriority::Critical);
    }

    #[test]
    fn test_reliability_updates() {
        let ai = AdaptiveReconnectionAI::new(ReconnectionConfig::default());

        // Record successes
        ai.record_connection_success("192.168.1.1", false, 100);
        ai.record_connection_success("192.168.1.1", false, 150);
        ai.record_connection_success("192.168.1.1", false, 120);

        let profile = ai.get_peer_stats("192.168.1.1").unwrap();
        assert!(profile.reliability_score > 0.8);
        assert_eq!(profile.consecutive_failures, 0);
    }

    #[test]
    fn test_backoff_on_failure() {
        let ai = AdaptiveReconnectionAI::new(ReconnectionConfig::default());

        // Record failures
        ai.record_connection_failure("192.168.1.1", false, "timeout");
        ai.record_connection_failure("192.168.1.1", false, "timeout");
        ai.record_connection_failure("192.168.1.1", false, "refused");

        let profile = ai.get_peer_stats("192.168.1.1").unwrap();
        assert_eq!(profile.consecutive_failures, 3);
        assert!(profile.optimal_retry_delay_secs > 5.0); // Should have increased
    }

    #[test]
    fn test_cooldown_period() {
        let config = ReconnectionConfig {
            max_consecutive_failures: 3,
            cooldown_period_secs: 60,
            ..Default::default()
        };
        let ai = AdaptiveReconnectionAI::new(config);

        // Trigger cooldown
        for _ in 0..5 {
            ai.record_connection_failure("192.168.1.1", false, "timeout");
        }

        let advice = ai.get_reconnection_advice("192.168.1.1", false);
        assert!(!advice.should_attempt);
        assert_eq!(advice.priority, ReconnectionPriority::Skip);
    }
}
