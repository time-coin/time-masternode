//! IP blacklisting for misbehaving peers.
//!
//! Phase 2.2: DoS Protection - IP Blacklisting
//! Tracks violations and automatically bans repeat offenders to prevent resource exhaustion.
//!
//! Bans are persisted to sled so they survive daemon restarts. Call
//! `attach_storage(db)` after construction to enable persistence.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// Banned IPv4 subnets (network_addr, prefix_len, reason) — e.g. 154.217.246.0/24
    subnet_blacklist: Vec<(std::net::Ipv4Addr, u8, String)>,
    // ── Sled persistence (None = in-memory only) ──────────────────────────
    db_permanent: Option<sled::Tree>,
    db_temp: Option<sled::Tree>,
    db_subnet: Option<sled::Tree>,
    db_violations: Option<sled::Tree>,
}

impl IPBlacklist {
    pub fn new() -> Self {
        Self {
            permanent_blacklist: HashMap::new(),
            temp_blacklist: HashMap::new(),
            violations: HashMap::new(),
            whitelist: HashMap::new(),
            subnet_blacklist: Vec::new(),
            db_permanent: None,
            db_temp: None,
            db_subnet: None,
            db_violations: None,
        }
    }

    /// Attach a sled database for persistence across restarts.
    ///
    /// Opens four named trees in `db`, loads any previously stored bans into
    /// the in-memory maps (pruning expired temp bans), and enables write-through
    /// on all future mutations.  Call this once after `new()`.
    pub fn attach_storage(&mut self, db: &sled::Db) {
        let open = |name: &str| {
            db.open_tree(name)
                .unwrap_or_else(|e| panic!("Failed to open sled tree {}: {}", name, e))
        };

        self.db_permanent = Some(open("ip_bans_permanent"));
        self.db_temp = Some(open("ip_bans_temp"));
        self.db_subnet = Some(open("ip_bans_subnet"));
        self.db_violations = Some(open("ip_bans_violations"));

        let now_unix = unix_now();

        // ── Load permanent bans ───────────────────────────────────────────
        if let Some(tree) = &self.db_permanent {
            let mut whitelisted_pruned = 0usize;
            for item in tree.iter().flatten() {
                let (k, v) = item;
                if let Ok(ip) = std::str::from_utf8(&k)
                    .ok()
                    .and_then(|s| s.parse::<IpAddr>().ok())
                    .ok_or(())
                {
                    if self.whitelist.contains_key(&ip) {
                        // Whitelisted peer — discard the stored ban and remove from sled
                        let _ = tree.remove(k);
                        whitelisted_pruned += 1;
                        tracing::info!(
                            "🔓 Cleared persisted ban for whitelisted peer {} on startup",
                            ip
                        );
                    } else {
                        let reason = String::from_utf8_lossy(&v).into_owned();
                        self.permanent_blacklist.insert(ip, reason);
                    }
                }
            }
            tracing::info!(
                "🔒 Loaded {} permanent IP ban(s) from sled ({} cleared for whitelisted peers)",
                self.permanent_blacklist.len(),
                whitelisted_pruned
            );
        }

        // ── Load temp bans (skip expired, skip whitelisted) ──────────────
        if let Some(tree) = &self.db_temp {
            let mut loaded = 0usize;
            let mut expired = 0usize;
            let mut whitelisted_pruned = 0usize;
            for item in tree.iter().flatten() {
                let (k, v) = item;
                if let Some(ip) = std::str::from_utf8(&k)
                    .ok()
                    .and_then(|s| s.parse::<IpAddr>().ok())
                {
                    if self.whitelist.contains_key(&ip) {
                        let _ = tree.remove(k);
                        whitelisted_pruned += 1;
                        tracing::info!(
                            "🔓 Cleared persisted temp ban for whitelisted peer {} on startup",
                            ip
                        );
                        continue;
                    }
                    if let Ok((expiry_unix, reason)) = bincode::deserialize::<(u64, String)>(&v) {
                        if expiry_unix <= now_unix {
                            // Expired — prune from sled too
                            let _ = tree.remove(k);
                            expired += 1;
                        } else {
                            let remaining = expiry_unix - now_unix;
                            let expiry = Instant::now() + Duration::from_secs(remaining);
                            self.temp_blacklist.insert(ip, (expiry, reason));
                            loaded += 1;
                        }
                    }
                }
            }
            if loaded > 0 || expired > 0 || whitelisted_pruned > 0 {
                tracing::info!(
                    "🔒 Loaded {} active temp ban(s) from sled ({} expired and pruned, {} cleared for whitelisted peers)",
                    loaded,
                    expired,
                    whitelisted_pruned
                );
            }
        }

        // ── Load subnet bans ─────────────────────────────────────────────
        if let Some(tree) = &self.db_subnet {
            for item in tree.iter().flatten() {
                let (k, v) = item;
                if let Ok(cidr) = std::str::from_utf8(&k) {
                    let reason = String::from_utf8_lossy(&v).into_owned();
                    // Parse CIDR into in-memory format
                    let (addr_str, prefix_len) = if let Some(pos) = cidr.find('/') {
                        let bits: u8 = cidr[pos + 1..].parse().unwrap_or(24);
                        (&cidr[..pos], bits)
                    } else {
                        (cidr, 24u8)
                    };
                    if let Ok(network) = addr_str.parse::<std::net::Ipv4Addr>() {
                        // Avoid duplicates (config may have already added some)
                        let cidr_str = format!("{}/{}", network, prefix_len);
                        let already = self
                            .subnet_blacklist
                            .iter()
                            .any(|(n, p, _)| format!("{}/{}", n, p) == cidr_str);
                        if !already {
                            self.subnet_blacklist.push((network, prefix_len, reason));
                        }
                    }
                }
            }
            tracing::info!(
                "🔒 Loaded {} subnet ban(s) from sled",
                self.subnet_blacklist.len()
            );
        }

        // ── Load violation counters (prune entries older than 1 hour, skip whitelisted) ────
        if let Some(tree) = &self.db_violations {
            let cutoff = now_unix.saturating_sub(3600);
            let mut loaded = 0usize;
            let mut whitelisted_pruned = 0usize;
            for item in tree.iter().flatten() {
                let (k, v) = item;
                if let Some(ip) = std::str::from_utf8(&k)
                    .ok()
                    .and_then(|s| s.parse::<IpAddr>().ok())
                {
                    if self.whitelist.contains_key(&ip) {
                        let _ = tree.remove(k);
                        whitelisted_pruned += 1;
                        continue;
                    }
                    if let Ok((count, last_unix)) = bincode::deserialize::<(u32, u64)>(&v) {
                        if last_unix < cutoff {
                            let _ = tree.remove(k);
                        } else {
                            // Reconstruct Instant from delta
                            let elapsed_secs = now_unix.saturating_sub(last_unix);
                            let last_instant =
                                Instant::now() - Duration::from_secs(elapsed_secs.min(3600));
                            self.violations.insert(ip, (count, last_instant));
                            loaded += 1;
                        }
                    }
                }
            }
            if loaded > 0 || whitelisted_pruned > 0 {
                tracing::info!(
                    "🔒 Loaded {} violation counter(s) from sled ({} cleared for whitelisted peers)",
                    loaded,
                    whitelisted_pruned
                );
            }
        }
    }

    // ── Private sled helpers ─────────────────────────────────────────────

    fn persist_permanent(&self, ip: IpAddr, reason: &str) {
        if let Some(tree) = &self.db_permanent {
            let _ = tree.insert(ip.to_string().as_bytes(), reason.as_bytes());
        }
    }

    fn remove_permanent(&self, ip: IpAddr) {
        if let Some(tree) = &self.db_permanent {
            let _ = tree.remove(ip.to_string().as_bytes());
        }
    }

    fn persist_temp(&self, ip: IpAddr, expiry_unix: u64, reason: &str) {
        if let Some(tree) = &self.db_temp {
            if let Ok(bytes) = bincode::serialize(&(expiry_unix, reason.to_string())) {
                let _ = tree.insert(ip.to_string().as_bytes(), bytes);
            }
        }
    }

    fn remove_temp(&self, ip: IpAddr) {
        if let Some(tree) = &self.db_temp {
            let _ = tree.remove(ip.to_string().as_bytes());
        }
    }

    fn persist_subnet(&self, cidr: &str, reason: &str) {
        if let Some(tree) = &self.db_subnet {
            let _ = tree.insert(cidr.as_bytes(), reason.as_bytes());
        }
    }

    fn persist_violation(&self, ip: IpAddr, count: u32) {
        if let Some(tree) = &self.db_violations {
            let now_unix = unix_now();
            if let Ok(bytes) = bincode::serialize(&(count, now_unix)) {
                let _ = tree.insert(ip.to_string().as_bytes(), bytes);
            }
        }
    }

    fn remove_violation(&self, ip: IpAddr) {
        if let Some(tree) = &self.db_violations {
            let _ = tree.remove(ip.to_string().as_bytes());
        }
    }

    /// Add an IP to the whitelist (exempt from all bans and rate limits)
    pub fn add_to_whitelist(&mut self, ip: IpAddr, reason: &str) {
        self.whitelist.insert(ip, reason.to_string());
        // Remove any existing bans or violations for whitelisted IPs
        self.permanent_blacklist.remove(&ip);
        self.temp_blacklist.remove(&ip);
        self.violations.remove(&ip);
        self.remove_permanent(ip);
        self.remove_temp(ip);
        self.remove_violation(ip);
        tracing::debug!("✅ Added {} to whitelist: {}", ip, reason);
    }

    /// Check if an IP is whitelisted
    pub fn is_whitelisted(&self, ip: IpAddr) -> bool {
        self.whitelist.contains_key(&ip)
    }

    /// Get whitelist count
    pub fn whitelist_count(&self) -> usize {
        self.whitelist.len()
    }

    /// Ban an entire IPv4 subnet in CIDR notation (e.g. "154.217.246.0/24").
    /// Also accepts a bare /24 prefix like "154.217.246" (prefix_len defaults to 24).
    pub fn add_subnet_ban(&mut self, cidr: &str, reason: &str) {
        let (addr_str, prefix_len) = if let Some(pos) = cidr.find('/') {
            let bits: u8 = cidr[pos + 1..].parse().unwrap_or(24);
            (&cidr[..pos], bits)
        } else {
            (cidr, 24u8)
        };
        if let Ok(network) = addr_str.parse::<std::net::Ipv4Addr>() {
            let canonical = format!("{}/{}", network, prefix_len);
            tracing::info!("🚫 Banning subnet {}: {}", canonical, reason);
            self.subnet_blacklist
                .push((network, prefix_len, reason.to_string()));
            self.persist_subnet(&canonical, reason);
        } else {
            tracing::warn!("⚠️  Invalid subnet CIDR '{}', skipping", cidr);
        }
    }

    /// Returns all currently banned subnets as CIDR strings (e.g. `"154.217.246.0/24"`).
    ///
    /// Used on startup to evict any Free-tier masternodes from subnets that were banned in
    /// a previous session (i.e. loaded back from persistent storage).
    pub fn list_banned_subnets(&self) -> Vec<String> {
        self.subnet_blacklist
            .iter()
            .map(|(network, prefix_len, _)| format!("{}/{}", network, prefix_len))
            .collect()
    }

    /// Returns true if `ip` falls within any banned subnet.
    fn in_banned_subnet(&self, ip: IpAddr) -> Option<String> {
        if let IpAddr::V4(v4) = ip {
            let ip_bits = u32::from(v4);
            for (network, prefix_len, reason) in &self.subnet_blacklist {
                let mask = if *prefix_len == 0 {
                    0u32
                } else {
                    !0u32 << (32 - *prefix_len as u32)
                };
                let net_bits = u32::from(*network);
                if (ip_bits & mask) == (net_bits & mask) {
                    return Some(format!(
                        "Subnet banned ({}/{}): {}",
                        network, prefix_len, reason
                    ));
                }
            }
        }
        None
    }

    /// Number of configured subnet bans
    pub fn subnet_ban_count(&self) -> usize {
        self.subnet_blacklist.len()
    }

    /// Check if an IP is currently blacklisted
    /// SECURITY: Blacklist takes precedence over whitelist
    pub fn is_blacklisted(&mut self, ip: IpAddr) -> Option<String> {
        // Check permanent blacklist FIRST (even for whitelisted IPs)
        if let Some(reason) = self.permanent_blacklist.get(&ip) {
            return Some(format!("Permanently banned: {}", reason));
        }

        // Check subnet blacklist
        if let Some(reason) = self.in_banned_subnet(ip) {
            return Some(reason);
        }

        // Check temporary blacklist and clean up expired entries
        if let Some((expiry, reason)) = self.temp_blacklist.get(&ip) {
            if Instant::now() < *expiry {
                let remaining = expiry.duration_since(Instant::now()).as_secs();
                return Some(format!("Temporarily banned for {}s: {}", remaining, reason));
            } else {
                // Expired, remove from memory and sled
                self.temp_blacklist.remove(&ip);
                self.remove_temp(ip);
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

        let count_snap = *count;

        // Only log at ban-trigger thresholds to avoid journal spam.
        // Per-message violation logging is done by the caller where context is richer.
        // Persist updated violation count
        self.persist_violation(ip, count_snap);

        // Auto-ban based on violation count
        match count_snap {
            3 => {
                // 3rd violation: 1 minute ban
                self.add_temp_ban(ip, Duration::from_secs(60), reason);
                tracing::warn!("🚫 Auto-banned {} for 1 minute (3 violations: {})", ip, reason);
                true
            }
            5 => {
                // 5th violation: 5 minute ban
                self.add_temp_ban(ip, Duration::from_secs(300), reason);
                tracing::warn!("🚫 Auto-banned {} for 5 minutes (5 violations: {})", ip, reason);
                true
            }
            10 => {
                // 10th violation: permanent ban
                self.add_permanent_ban(ip, reason);
                tracing::warn!("🚫 PERMANENTLY BANNED {} (10 violations: {})", ip, reason);
                true
            }
            1 | 2 | 4 | 6..=9 => {
                tracing::debug!("⚠️  Violation #{} from {}: {}", count_snap, ip, reason);
                false
            }
            _ => {
                // Already permanently banned, disconnect
                true
            }
        }
    }

    /// Record a TLS-layer connection failure.
    ///
    /// TLS mode mismatches (e.g., a node configured for TLS connecting to a plaintext
    /// listener) are not malicious — they are operator configuration errors.  Using the
    /// standard `record_violation` path would permanently ban a legitimate node after
    /// 10 retries.  This method uses a much higher threshold and caps at a 1-hour
    /// temporary ban, never escalating to permanent.
    ///
    /// Returns true if the IP should be disconnected (i.e., a ban was just applied).
    pub fn record_tls_violation(&mut self, ip: IpAddr, reason: &str) -> bool {
        // Whitelisted IPs are always exempt.
        if self.is_whitelisted(ip) {
            return false;
        }

        let now = Instant::now();
        let (count, last_time) = self.violations.entry(ip).or_insert((0, now));

        // Reset after 1 hour of quiet.
        if now.duration_since(*last_time) > Duration::from_secs(3600) {
            *count = 0;
        }

        *count += 1;
        *last_time = now;

        let count_snap = *count;
        self.persist_violation(ip, count_snap);

        // Much more lenient thresholds than record_violation, and NEVER permanent.
        match count_snap {
            10 => {
                self.add_temp_ban(ip, Duration::from_secs(300), reason);
                tracing::warn!(
                    "🚫 TLS: temp-banned {} for 5 minutes (10 TLS failures: {})",
                    ip, reason
                );
                true
            }
            30 => {
                self.add_temp_ban(ip, Duration::from_secs(3600), reason);
                tracing::warn!(
                    "🚫 TLS: temp-banned {} for 1 hour (30 TLS failures: {})",
                    ip, reason
                );
                true
            }
            1..=9 | 11..=29 => {
                tracing::debug!("⚠️  TLS failure #{} from {}: {}", count_snap, ip, reason);
                false
            }
            _ => {
                // At >30 TLS failures just keep the 1-hour ban cycling — never permanent.
                if count_snap % 30 == 0 {
                    self.add_temp_ban(ip, Duration::from_secs(3600), reason);
                    tracing::warn!(
                        "🚫 TLS: renewed 1-hour ban for {} ({} total TLS failures)",
                        ip, count_snap
                    );
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Add a temporary ban
    pub fn add_temp_ban(&mut self, ip: IpAddr, duration: Duration, reason: &str) {
        let expiry = Instant::now() + duration;
        self.temp_blacklist.insert(ip, (expiry, reason.to_string()));
        // Persist: convert to unix timestamp so it survives restarts
        let expiry_unix = unix_now() + duration.as_secs();
        self.persist_temp(ip, expiry_unix, reason);
    }

    /// Add a permanent ban
    pub fn add_permanent_ban(&mut self, ip: IpAddr, reason: &str) {
        self.permanent_blacklist.insert(ip, reason.to_string());
        self.temp_blacklist.remove(&ip);
        self.persist_permanent(ip, reason);
        self.remove_temp(ip);
    }

    /// Clean up expired temporary bans and old violations (call periodically)
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        let now_unix = unix_now();

        // Remove expired temp bans (and prune from sled)
        let expired_ips: Vec<IpAddr> = self
            .temp_blacklist
            .iter()
            .filter(|(_, (expiry, _))| now >= *expiry)
            .map(|(ip, _)| *ip)
            .collect();
        for ip in &expired_ips {
            self.temp_blacklist.remove(ip);
            self.remove_temp(*ip);
        }

        // Remove violations older than 24 hours (and prune from sled)
        let old_ips: Vec<IpAddr> = self
            .violations
            .iter()
            .filter(|(_, (_, last_time))| {
                now.duration_since(*last_time) >= Duration::from_secs(86400)
            })
            .map(|(ip, _)| *ip)
            .collect();
        for ip in &old_ips {
            self.violations.remove(ip);
            self.remove_violation(*ip);
        }

        let _ = now_unix; // used indirectly via remove_temp/remove_violation
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

    /// List all active bans with details.
    /// Returns (permanent_bans, temp_bans_with_remaining_secs, subnet_bans, violations_per_ip)
    #[allow(clippy::type_complexity)]
    pub fn list_bans(
        &self,
    ) -> (
        Vec<(String, String)>,      // (ip, reason)
        Vec<(String, u64, String)>, // (ip, remaining_secs, reason)
        Vec<(String, String)>,      // (cidr, reason)
        Vec<(String, u32)>,         // (ip, violation_count)
    ) {
        let now = Instant::now();

        let permanent: Vec<(String, String)> = self
            .permanent_blacklist
            .iter()
            .map(|(ip, reason)| (ip.to_string(), reason.clone()))
            .collect();

        let temporary: Vec<(String, u64, String)> = self
            .temp_blacklist
            .iter()
            .filter(|(_, (expiry, _))| now < *expiry)
            .map(|(ip, (expiry, reason))| {
                let remaining = expiry.duration_since(now).as_secs();
                (ip.to_string(), remaining, reason.clone())
            })
            .collect();

        let subnets: Vec<(String, String)> = self
            .subnet_blacklist
            .iter()
            .map(|(net, prefix, reason)| (format!("{}/{}", net, prefix), reason.clone()))
            .collect();

        let violations: Vec<(String, u32)> = self
            .violations
            .iter()
            .map(|(ip, (count, _))| (ip.to_string(), *count))
            .collect();

        (permanent, temporary, subnets, violations)
    }

    /// Remove an IP from permanent and temporary bans, and clear its violations.
    /// Returns true if the IP was actually banned (and is now cleared).
    pub fn unban(&mut self, ip: IpAddr) -> bool {
        let was_banned = self.permanent_blacklist.remove(&ip).is_some()
            | self.temp_blacklist.remove(&ip).is_some();
        self.violations.remove(&ip);
        self.remove_permanent(ip);
        self.remove_temp(ip);
        self.remove_violation(ip);
        was_banned
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

        let count_snap = *count;
        tracing::error!(
            "🚨 SEVERE violation from {}: {} (effective count: {})",
            ip,
            reason,
            count_snap
        );

        // Persist updated count
        self.persist_violation(ip, count_snap);

        // Immediate escalation for severe violations
        if count_snap >= 10 {
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

/// Returns seconds since UNIX_EPOCH (u64). Used to store `Instant`-based expiries
/// as absolute wall-clock timestamps so they survive daemon restarts.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
