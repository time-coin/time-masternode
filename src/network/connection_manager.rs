//! Connection manager for tracking peer connection state
//! Uses DashMap for lock-free concurrent access to connection states
//!
//! Phase 2.1: Enhanced with connection limits, rate limiting, and quality tracking

#![allow(dead_code)] // Public API - methods will be used by server and monitoring

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

// Phase 2.1: Connection limits for DoS protection
// Phase 3: Masternode slot reservation
const MAX_TOTAL_CONNECTIONS: usize = 125;
const MAX_INBOUND_CONNECTIONS: usize = 100;
const MAX_OUTBOUND_CONNECTIONS: usize = 25;
const RESERVED_MASTERNODE_SLOTS: usize = 50; // Reserve slots for whitelisted masternodes
const MAX_REGULAR_PEER_CONNECTIONS: usize = 75; // Remaining slots for regular peers
const MAX_CONNECTIONS_PER_IP: usize = 3;
const CONNECTION_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60); // 1 minute
const MAX_NEW_CONNECTIONS_PER_WINDOW: usize = 10; // 10 new connections per minute

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

#[derive(Clone, Debug)]
struct ConnectionInfo {
    state: PeerConnectionState,
    direction: ConnectionDirection,
    connected_at: Option<Instant>,
    disconnected_at: Option<Instant>,
    connection_count: usize, // Track how many times this IP has connected
    last_message_at: Option<Instant>, // For detecting slow/unresponsive peers
    bytes_sent: u64,
    bytes_received: u64,
    messages_sent: u64,
    messages_received: u64,
    is_whitelisted: bool, // NEW: Track if this is a whitelisted masternode
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionDirection {
    Inbound,
    Outbound,
}

/// Manages the lifecycle of peer connections (inbound/outbound)
pub struct ConnectionManager {
    connections: Arc<DashMap<String, ConnectionInfo>>,
    connected_count: Arc<std::sync::atomic::AtomicUsize>,
    inbound_count: Arc<std::sync::atomic::AtomicUsize>,
    outbound_count: Arc<std::sync::atomic::AtomicUsize>,
    // Phase 2.1: Connection rate limiting
    recent_connections: Arc<DashMap<Instant, String>>, // timestamp -> peer_ip
    last_cleanup: Arc<std::sync::Mutex<Instant>>,
}

impl ConnectionManager {
    /// Create a new connection manager
    pub fn new() -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            connected_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            inbound_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            outbound_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            recent_connections: Arc::new(DashMap::new()),
            last_cleanup: Arc::new(std::sync::Mutex::new(Instant::now())),
        }
    }

    /// Phase 2.1: Check if we can accept a new inbound connection
    /// Phase 3: Add whitelist exemption for masternodes
    pub fn can_accept_inbound(&self, peer_ip: &str, is_whitelisted: bool) -> Result<(), String> {
        // Whitelisted masternodes bypass regular connection limits (but still respect total)
        if is_whitelisted {
            let total = self.connected_count();
            if total >= MAX_TOTAL_CONNECTIONS {
                return Err(format!(
                    "Max total connections reached: {}/{}",
                    total, MAX_TOTAL_CONNECTIONS
                ));
            }
            // Allow whitelisted connection - bypass all other checks
            return Ok(());
        }

        // For regular peers, enforce stricter limits
        let total = self.connected_count();
        if total >= MAX_TOTAL_CONNECTIONS {
            return Err(format!(
                "Max total connections reached: {}/{}",
                total, MAX_TOTAL_CONNECTIONS
            ));
        }

        // Check if regular peer slots are full
        let regular_count = self.count_regular_peer_connections();
        if regular_count >= MAX_REGULAR_PEER_CONNECTIONS {
            return Err(format!(
                "Max regular peer connections reached: {}/{} (reserved {} slots for masternodes)",
                regular_count, MAX_REGULAR_PEER_CONNECTIONS, RESERVED_MASTERNODE_SLOTS
            ));
        }

        // Check inbound limit
        let inbound = self
            .inbound_count
            .load(std::sync::atomic::Ordering::Relaxed);
        if inbound >= MAX_INBOUND_CONNECTIONS {
            return Err(format!(
                "Max inbound connections reached: {}/{}",
                inbound, MAX_INBOUND_CONNECTIONS
            ));
        }

        // Check per-IP limit
        let ip_connections = self.count_connections_from_ip(peer_ip);
        if ip_connections >= MAX_CONNECTIONS_PER_IP {
            return Err(format!(
                "Max connections per IP reached: {}/{}",
                ip_connections, MAX_CONNECTIONS_PER_IP
            ));
        }

        // Phase 2.1: Check connection rate limit
        self.cleanup_old_connections();
        let recent_count = self.recent_connections.len();
        if recent_count >= MAX_NEW_CONNECTIONS_PER_WINDOW {
            return Err(format!(
                "Connection rate limit exceeded: {} connections in last minute",
                recent_count
            ));
        }

        Ok(())
    }

    /// Phase 2.1: Check if we can make a new outbound connection
    pub fn can_connect_outbound(&self) -> Result<(), String> {
        // Check total connection limit
        let total = self.connected_count();
        if total >= MAX_TOTAL_CONNECTIONS {
            return Err(format!(
                "Max total connections reached: {}/{}",
                total, MAX_TOTAL_CONNECTIONS
            ));
        }

        // Check outbound limit
        let outbound = self
            .outbound_count
            .load(std::sync::atomic::Ordering::Relaxed);
        if outbound >= MAX_OUTBOUND_CONNECTIONS {
            return Err(format!(
                "Max outbound connections reached: {}/{}",
                outbound, MAX_OUTBOUND_CONNECTIONS
            ));
        }

        Ok(())
    }

    /// Phase 2.1: Count connections from a specific IP
    fn count_connections_from_ip(&self, peer_ip: &str) -> usize {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().starts_with(peer_ip)
                    && entry.value().state == PeerConnectionState::Connected
            })
            .count()
    }

    /// Phase 3: Count regular (non-whitelisted) peer connections
    fn count_regular_peer_connections(&self) -> usize {
        self.connections
            .iter()
            .filter(|entry| {
                entry.value().state == PeerConnectionState::Connected
                    && !entry.value().is_whitelisted
            })
            .count()
    }

    /// Phase 3: Count whitelisted masternode connections
    #[allow(dead_code)]
    pub fn count_whitelisted_connections(&self) -> usize {
        self.connections
            .iter()
            .filter(|entry| {
                entry.value().state == PeerConnectionState::Connected
                    && entry.value().is_whitelisted
            })
            .count()
    }

    /// Phase 2.1: Cleanup old connection tracking entries
    fn cleanup_old_connections(&self) {
        let mut last_cleanup = self.last_cleanup.lock().unwrap();
        let now = Instant::now();

        // Only cleanup every 10 seconds
        if now.duration_since(*last_cleanup) < Duration::from_secs(10) {
            return;
        }

        let cutoff = now - CONNECTION_RATE_LIMIT_WINDOW;
        self.recent_connections
            .retain(|timestamp, _| *timestamp > cutoff);

        *last_cleanup = now;
    }

    /// Phase 2.1: Record a new connection for rate limiting
    fn record_new_connection(&self, peer_ip: &str) {
        self.recent_connections
            .insert(Instant::now(), peer_ip.to_string());
    }

    /// Check if we're already connected to a peer
    pub fn is_connected(&self, peer_ip: &str) -> bool {
        self.connections
            .get(peer_ip)
            .map(|info| info.state == PeerConnectionState::Connected)
            .unwrap_or(false)
    }

    /// Check if a peer has any active state (connecting, connected, or reconnecting)
    pub fn is_active(&self, peer_ip: &str) -> bool {
        self.connections
            .get(peer_ip)
            .map(|info| info.state != PeerConnectionState::Disconnected)
            .unwrap_or(false)
    }

    /// Check if we have an outbound connection to a peer
    pub fn has_outbound_connection(&self, peer_ip: &str) -> bool {
        self.connections
            .get(peer_ip)
            .map(|info| {
                info.state == PeerConnectionState::Connected
                    && info.direction == ConnectionDirection::Outbound
            })
            .unwrap_or(false)
    }

    /// Check if we should connect to a peer
    /// Returns false if already connected or currently connecting
    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        !self.connections.contains_key(peer_ip)
            || self
                .connections
                .get(peer_ip)
                .map(|info| info.state == PeerConnectionState::Disconnected)
                .unwrap_or(false)
    }

    /// Mark a peer as connected (inbound connection)
    pub fn mark_inbound(&self, peer_ip: &str) -> bool {
        self.record_new_connection(peer_ip);

        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            if entry.state == PeerConnectionState::Connecting {
                entry.state = PeerConnectionState::Connected;
                entry.connected_at = Some(Instant::now());
                entry.connection_count += 1;

                self.connected_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.inbound_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                true
            } else {
                false
            }
        } else {
            let info = ConnectionInfo {
                state: PeerConnectionState::Connected,
                direction: ConnectionDirection::Inbound,
                connected_at: Some(Instant::now()),
                disconnected_at: None,
                connection_count: 1,
                last_message_at: Some(Instant::now()),
                bytes_sent: 0,
                bytes_received: 0,
                messages_sent: 0,
                messages_received: 0,
                is_whitelisted: false, // Will be updated if peer is whitelisted
            };
            self.connections.insert(peer_ip.to_string(), info);
            self.connected_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inbound_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        }
    }

    /// Mark an inbound connection as disconnected
    pub fn mark_inbound_disconnected(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            if entry.state == PeerConnectionState::Connected {
                entry.state = PeerConnectionState::Disconnected;
                entry.disconnected_at = Some(Instant::now());

                self.connected_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                if entry.direction == ConnectionDirection::Inbound {
                    self.inbound_count
                        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                }
                return true;
            }
        }
        false
    }

    /// Mark a peer as being attempted for connection
    pub fn mark_connecting(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            // Allow reconnection from Disconnected state
            if entry.state == PeerConnectionState::Disconnected {
                entry.state = PeerConnectionState::Connecting;
                entry.direction = ConnectionDirection::Outbound;
                entry.last_message_at = Some(Instant::now()); // Track when connecting started
                true
            } else {
                false
            }
        } else {
            let info = ConnectionInfo {
                state: PeerConnectionState::Connecting,
                direction: ConnectionDirection::Outbound,
                connected_at: None,
                disconnected_at: None,
                connection_count: 0,
                last_message_at: Some(Instant::now()), // Track when connecting started
                bytes_sent: 0,
                bytes_received: 0,
                messages_sent: 0,
                messages_received: 0,
                is_whitelisted: false,
            };
            self.connections.insert(peer_ip.to_string(), info);
            true
        }
    }

    /// Mark a connection attempt as successfully connected
    pub fn mark_connected(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            if entry.state == PeerConnectionState::Connecting {
                entry.state = PeerConnectionState::Connected;
                entry.connected_at = Some(Instant::now());
                entry.last_message_at = Some(Instant::now());
                entry.connection_count += 1;

                self.connected_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                if entry.direction == ConnectionDirection::Outbound {
                    self.outbound_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Mark a connection as failed and retry later with backoff
    pub fn mark_failed(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            entry.state = PeerConnectionState::Reconnecting;
            entry.disconnected_at = Some(Instant::now());
            true
        } else {
            let info = ConnectionInfo {
                state: PeerConnectionState::Reconnecting,
                direction: ConnectionDirection::Outbound,
                connected_at: None,
                disconnected_at: Some(Instant::now()),
                connection_count: 0,
                last_message_at: None,
                bytes_sent: 0,
                bytes_received: 0,
                messages_sent: 0,
                messages_received: 0,
                is_whitelisted: false,
            };
            self.connections.insert(peer_ip.to_string(), info);
            true
        }
    }

    /// Remove a peer from tracking (cleanup)
    pub fn remove(&self, peer_ip: &str) {
        if let Some((_, info)) = self.connections.remove(peer_ip) {
            if info.state == PeerConnectionState::Connected {
                self.connected_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                match info.direction {
                    ConnectionDirection::Inbound => {
                        self.inbound_count
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    ConnectionDirection::Outbound => {
                        self.outbound_count
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }

    /// Get count of connected peers
    pub fn connected_count(&self) -> usize {
        self.connected_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Phase 2.1: Get inbound connection count
    pub fn inbound_count(&self) -> usize {
        self.inbound_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Phase 2.1: Get outbound connection count
    pub fn outbound_count(&self) -> usize {
        self.outbound_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Check if a peer is in reconnecting state
    pub fn is_reconnecting(&self, peer_ip: &str) -> bool {
        self.connections
            .get(peer_ip)
            .map(|info| info.state == PeerConnectionState::Reconnecting)
            .unwrap_or(false)
    }

    /// Mark a peer as disconnected
    pub fn mark_disconnected(&self, peer_ip: &str) {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            if entry.state == PeerConnectionState::Connected {
                self.connected_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

                match entry.direction {
                    ConnectionDirection::Inbound => {
                        self.inbound_count
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    ConnectionDirection::Outbound => {
                        self.outbound_count
                            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            entry.state = PeerConnectionState::Disconnected;
            entry.disconnected_at = Some(Instant::now());
        }
    }

    /// Clear reconnecting state for a peer (allow immediate retry)
    pub fn clear_reconnecting(&self, peer_ip: &str) {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            if entry.state == PeerConnectionState::Reconnecting {
                entry.state = PeerConnectionState::Disconnected;
            }
        }
    }

    /// Phase 3: Mark a connection as whitelisted (trusted masternode)
    pub fn mark_whitelisted(&self, peer_ip: &str) {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            entry.is_whitelisted = true;
        }
    }

    /// Phase 3: Check if a peer is marked as whitelisted
    pub fn is_whitelisted(&self, peer_ip: &str) -> bool {
        self.connections
            .get(peer_ip)
            .map(|entry| entry.is_whitelisted)
            .unwrap_or(false)
    }

    /// Phase 2: Check if peer should be protected from disconnection (whitelisted)
    pub fn should_protect(&self, peer_ip: &str) -> bool {
        self.is_whitelisted(peer_ip)
    }

    /// Mark a peer as reconnecting (with retry logic)
    pub fn mark_reconnecting(
        &self,
        peer_ip: &str,
        _retry_delay: std::time::Duration,
        _consecutive_failures: u32,
    ) {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            entry.state = PeerConnectionState::Reconnecting;
        }
    }

    /// Get list of currently connected peers
    pub fn get_connected_peers(&self) -> Vec<String> {
        self.connections
            .iter()
            .filter(|entry| entry.value().state == PeerConnectionState::Connected)
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get list of peers currently being connected to
    pub fn get_connecting_peers(&self) -> Vec<String> {
        self.connections
            .iter()
            .filter(|entry| entry.value().state == PeerConnectionState::Connecting)
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Reset peers stuck in Connecting/Reconnecting state for longer than the timeout.
    /// Returns the number of peers reset to Disconnected.
    pub fn cleanup_stale_connecting(&self, timeout: Duration) -> usize {
        let now = Instant::now();
        let mut cleaned = 0;
        for mut entry in self.connections.iter_mut() {
            let key = entry.key().clone();
            let info = entry.value_mut();
            if info.state == PeerConnectionState::Connecting
                || info.state == PeerConnectionState::Reconnecting
            {
                let started = info.last_message_at.unwrap_or(now);
                if now.duration_since(started) > timeout {
                    tracing::debug!("🧹 Resetting stale {:?} state for peer {}", info.state, key);
                    info.state = PeerConnectionState::Disconnected;
                    info.disconnected_at = Some(now);
                    cleaned += 1;
                }
            }
        }
        cleaned
    }

    /// Phase 2.1: Update activity timestamp for a peer (for detecting slow/unresponsive)
    pub fn record_activity(&self, peer_ip: &str) {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            entry.last_message_at = Some(Instant::now());
        }
    }

    /// Phase 2.1: Check for slow/unresponsive peers (no activity in 5 minutes)
    pub fn get_unresponsive_peers(&self, timeout: Duration) -> Vec<String> {
        let now = Instant::now();
        self.connections
            .iter()
            .filter(|entry| {
                entry.value().state == PeerConnectionState::Connected
                    && entry
                        .value()
                        .last_message_at
                        .map(|last| now.duration_since(last) > timeout)
                        .unwrap_or(false)
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Phase 2.1: Get connection quality metrics for a peer
    pub fn get_connection_quality(&self, peer_ip: &str) -> Option<ConnectionQuality> {
        self.connections.get(peer_ip).map(|info| {
            let uptime = info
                .connected_at
                .map(|connected| Instant::now().duration_since(connected))
                .unwrap_or(Duration::from_secs(0));

            let messages_per_sec = if uptime.as_secs() > 0 {
                (info.messages_received + info.messages_sent) as f64 / uptime.as_secs() as f64
            } else {
                0.0
            };

            ConnectionQuality {
                uptime,
                connection_count: info.connection_count,
                messages_per_sec,
                bytes_sent: info.bytes_sent,
                bytes_received: info.bytes_received,
            }
        })
    }
}

/// Phase 2.1: Connection quality metrics
#[derive(Debug, Clone)]
pub struct ConnectionQuality {
    pub uptime: Duration,
    pub connection_count: usize,
    pub messages_per_sec: f64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}

impl ConnectionManager {
    /// Update whitelist status for a specific peer
    /// Call this when masternode registry changes
    pub fn update_whitelist_status(&self, peer_ip: &str, is_whitelisted: bool) {
        if let Some(mut entry) = self.connections.get_mut(peer_ip) {
            let old_status = entry.is_whitelisted;
            entry.is_whitelisted = is_whitelisted;

            if old_status != is_whitelisted {
                tracing::info!(
                    "🔄 Updated whitelist status for {}: {} → {}",
                    peer_ip,
                    old_status,
                    is_whitelisted
                );
            }
        }
    }

    /// Bulk sync whitelist status from masternode registry
    /// Call this periodically or when masternode set changes
    ///
    /// # Arguments
    /// * `masternode_ips` - List of current masternode IP addresses
    pub fn sync_whitelist_from_registry(&self, masternode_ips: &[String]) {
        use std::collections::HashSet;

        let whitelist_set: HashSet<_> = masternode_ips.iter().map(|s| s.as_str()).collect();
        let mut updated_count = 0;

        for mut entry in self.connections.iter_mut() {
            let ip = entry.key().as_str();
            let should_be_whitelisted = whitelist_set.contains(ip);

            if entry.is_whitelisted != should_be_whitelisted {
                entry.is_whitelisted = should_be_whitelisted;
                updated_count += 1;
            }
        }

        if updated_count > 0 {
            tracing::info!(
                "🔄 Synced whitelist status: updated {} connections from masternode registry",
                updated_count
            );
        }
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}
