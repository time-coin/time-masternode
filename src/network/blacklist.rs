//! IP blacklisting for misbehaving peers.
//!
//! Phase 2.2: DoS Protection - IP Blacklisting
//! Tracks violations and automatically bans repeat offenders to prevent resource exhaustion.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Tracks misbehaving IPs and automatically blacklists repeat offenders
pub struct IPBlacklist {
    /// Permanently blacklisted IPs
    permanent_blacklist: HashMap<IpAddr, String>,
    /// Temporarily blacklisted IPs with expiry time
    temp_blacklist: HashMap<IpAddr, (Instant, String)>,
    /// Violation tracking: IP -> (violation_count, last_violation_time)
    violations: HashMap<IpAddr, (u32, Instant)>,
    /// Whitelisted IPs (exempt from all bans and rate limits) - typically masternodes
    whitelist: HashMap<IpAddr, String>,
}

impl IPBlacklist {
    pub fn new() -> Self {
        Self {
            permanent_blacklist: HashMap::new(),
            temp_blacklist: HashMap::new(),
            violations: HashMap::new(),
            whitelist: HashMap::new(),
        }
    }

    /// Add an IP to the whitelist (exempt from all bans and rate limits)
    pub fn add_to_whitelist(&mut self, ip: IpAddr, reason: &str) {
        self.whitelist.insert(ip, reason.to_string());
        // Remove any existing bans or violations for whitelisted IPs
        self.permanent_blacklist.remove(&ip);
        self.temp_blacklist.remove(&ip);
        self.violations.remove(&ip);
        tracing::info!("✅ Added {} to whitelist: {}", ip, reason);
    }

    /// Check if an IP is whitelisted
    pub fn is_whitelisted(&self, ip: IpAddr) -> bool {
        self.whitelist.contains_key(&ip)
    }

    /// Get whitelist count
    pub fn whitelist_count(&self) -> usize {
        self.whitelist.len()
    }

    /// Check if an IP is currently blacklisted
    /// SECURITY: Blacklist takes precedence over whitelist
    pub fn is_blacklisted(&mut self, ip: IpAddr) -> Option<String> {
        // Check permanent blacklist FIRST (even for whitelisted IPs)
        if let Some(reason) = self.permanent_blacklist.get(&ip) {
            return Some(format!("Permanently banned: {}", reason));
        }

        // Check temporary blacklist and clean up expired entries
        if let Some((expiry, reason)) = self.temp_blacklist.get(&ip) {
            if Instant::now() < *expiry {
                let remaining = expiry.duration_since(Instant::now()).as_secs();
                return Some(format!("Temporarily banned for {}s: {}", remaining, reason));
            } else {
                // Expired, remove it
                self.temp_blacklist.remove(&ip);
            }
        }

        None
    }

    /// Record a violation for an IP
    /// Returns true if the IP should be disconnected (auto-banned)
    /// Note: Minor violations are still exempt for whitelisted IPs
    /// Use record_severe_violation for security issues that should apply to everyone
    pub fn record_violation(&mut self, ip: IpAddr, reason: &str) -> bool {
        // Minor violations: whitelisted IPs are exempt
        if self.is_whitelisted(ip) {
            tracing::debug!(
                "⚪ Ignoring minor violation for whitelisted IP {}: {}",
                ip,
                reason
            );
            return false;
        }

        let now = Instant::now();

        // Get or create violation record
        let (count, last_time) = self.violations.entry(ip).or_insert((0, now));

        // Reset count if last violation was over 1 hour ago
        if now.duration_since(*last_time) > Duration::from_secs(3600) {
            *count = 0;
        }

        *count += 1;
        *last_time = now;

        tracing::warn!("⚠️  Violation #{} from {}: {}", count, ip, reason);

        // Auto-ban based on violation count
        match *count {
            3 => {
                // 3rd violation: 5 minute ban
                self.add_temp_ban(ip, Duration::from_secs(300), reason);
                tracing::warn!("🚫 Auto-banned {} for 5 minutes (3 violations)", ip);
                true
            }
            5 => {
                // 5th violation: 1 hour ban
                self.add_temp_ban(ip, Duration::from_secs(3600), reason);
                tracing::warn!("🚫 Auto-banned {} for 1 hour (5 violations)", ip);
                true
            }
            10 => {
                // 10th violation: permanent ban
                self.add_permanent_ban(ip, reason);
                tracing::warn!("🚫 PERMANENTLY BANNED {} (10 violations)", ip);
                true
            }
            1 | 2 | 4 | 6..=9 => {
                // Warning only, don't disconnect yet
                false
            }
            _ => {
                // Already permanently banned, disconnect
                true
            }
        }
    }

    /// Add a temporary ban
    pub fn add_temp_ban(&mut self, ip: IpAddr, duration: Duration, reason: &str) {
        let expiry = Instant::now() + duration;
        self.temp_blacklist.insert(ip, (expiry, reason.to_string()));
    }

    /// Add a permanent ban
    pub fn add_permanent_ban(&mut self, ip: IpAddr, reason: &str) {
        self.permanent_blacklist.insert(ip, reason.to_string());
        // Remove from temp list if present
        self.temp_blacklist.remove(&ip);
    }

    /// Clean up expired temporary bans and old violations (call periodically)
    pub fn cleanup(&mut self) {
        let now = Instant::now();

        // Remove expired temp bans
        self.temp_blacklist.retain(|_, (expiry, _)| now < *expiry);

        // Remove violations older than 24 hours
        self.violations.retain(|_, (_, last_time)| {
            now.duration_since(*last_time) < Duration::from_secs(86400)
        });
    }

    /// Get statistics
    #[allow(dead_code)]
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.permanent_blacklist.len(),
            self.temp_blacklist.len(),
            self.violations.len(),
            self.whitelist.len(),
        )
    }

    /// Record a SEVERE violation (corrupted blocks, invalid chain data, reorg attacks)
    /// These are treated more harshly - immediate 1-hour ban on first offense,
    /// permanent ban on second offense
    /// SECURITY: Severe violations apply even to whitelisted peers (blacklist overrides whitelist)
    /// Returns true if the IP should be disconnected
    pub fn record_severe_violation(&mut self, ip: IpAddr, reason: &str) -> bool {
        let is_whitelisted = self.is_whitelisted(ip);

        if is_whitelisted {
            tracing::warn!(
                "🛡️ SECURITY: Recording severe violation for WHITELISTED peer {} - blacklist will override: {}",
                ip,
                reason
            );
        }

        let now = Instant::now();

        // Get or create violation record
        let (count, _) = self.violations.entry(ip).or_insert((0, now));
        *count += 5; // Severe violations count as 5 regular violations

        tracing::error!(
            "🚨 SEVERE violation from {}: {} (effective count: {})",
            ip,
            reason,
            count
        );

        // Immediate escalation for severe violations
        if *count >= 10 {
            self.add_permanent_ban(ip, &format!("SEVERE: {}", reason));
            tracing::error!(
                "🚫 PERMANENTLY BANNED {} for severe violation: {}",
                ip,
                reason
            );
            true
        } else {
            // First severe violation: 1 hour ban
            self.add_temp_ban(
                ip,
                Duration::from_secs(3600),
                &format!("SEVERE: {}", reason),
            );
            tracing::warn!("🚫 Banned {} for 1 hour (severe violation): {}", ip, reason);
            true
        }
    }
}

impl Default for IPBlacklist {
    fn default() -> Self {
        Self::new()
    }
}
