use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    Inbound,
    Outbound,
}

#[derive(Clone)]
struct ConnectionState {
    direction: ConnectionDirection,
    #[allow(dead_code)]
    connected_at: Instant,
}

/// State for tracking reconnection backoff
#[derive(Clone)]
struct ReconnectionState {
    next_attempt: Instant,
    #[allow(dead_code)]
    attempt_count: u64,
}

/// Tracks which IPs we have active connections to (prevents duplicate connections)
/// Uses lock-free concurrent access with DashMap and atomic counters
pub struct ConnectionManager {
    // Single map for all connections with direction tracking
    connections: DashMap<String, ConnectionState>,
    // Track reconnection backoff
    reconnecting: DashMap<String, ReconnectionState>,
    // Local IP - set once, read many (lock-free with ArcSwapOption)
    local_ip: ArcSwapOption<String>,
    // Metrics (atomic, no locks)
    inbound_count: AtomicUsize,
    outbound_count: AtomicUsize,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            reconnecting: DashMap::new(),
            local_ip: ArcSwapOption::empty(),
            inbound_count: AtomicUsize::new(0),
            outbound_count: AtomicUsize::new(0),
        }
    }

    /// Set our local IP address for deterministic connection direction (call once at startup)
    pub fn set_local_ip(&self, ip: String) {
        self.local_ip.store(Some(Arc::new(ip)));
    }

    /// Determine if we should initiate connection based on IP comparison
    /// Returns true if our IP is "higher" than peer IP (we should connect)
    /// Returns false if peer IP is "higher" (they should connect to us)
    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        let local_ip_guard = self.local_ip.load();

        if let Some(local_ip_arc) = local_ip_guard.as_ref() {
            let local_ip = local_ip_arc.as_str();

            // Parse both IPs for comparison
            if let (Ok(local_addr), Ok(peer_addr)) =
                (local_ip.parse::<IpAddr>(), peer_ip.parse::<IpAddr>())
            {
                // Compare as bytes to get deterministic ordering
                match (local_addr, peer_addr) {
                    (IpAddr::V4(l), IpAddr::V4(p)) => l.octets() > p.octets(),
                    (IpAddr::V6(l), IpAddr::V6(p)) => l.octets() > p.octets(),
                    // Mixed v4/v6: v6 > v4
                    (IpAddr::V6(_), IpAddr::V4(_)) => true,
                    (IpAddr::V4(_), IpAddr::V6(_)) => false,
                }
            } else {
                // Fallback to string comparison if parsing fails
                local_ip > peer_ip
            }
        } else {
            // If we don't know our IP, allow connection
            true
        }
    }

    /// Mark that we're connecting to this IP (outbound)
    /// Returns true if successfully marked (wasn't already connecting)
    pub fn mark_connecting(&self, ip: &str) -> bool {
        use dashmap::mapref::entry::Entry;

        match self.connections.entry(ip.to_string()) {
            Entry::Vacant(e) => {
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Outbound,
                    connected_at: Instant::now(),
                });
                self.outbound_count.fetch_add(1, Ordering::Relaxed);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Check if we're already connected/connecting to this IP (lock-free)
    pub fn is_connected(&self, ip: &str) -> bool {
        self.connections.contains_key(ip)
    }

    /// Mark an inbound connection
    pub fn mark_inbound(&self, ip: &str) -> bool {
        use dashmap::mapref::entry::Entry;

        match self.connections.entry(ip.to_string()) {
            Entry::Vacant(e) => {
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Inbound,
                    connected_at: Instant::now(),
                });
                self.inbound_count.fetch_add(1, Ordering::Relaxed);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Get connection direction
    #[allow(dead_code)]
    pub fn get_direction(&self, ip: &str) -> Option<ConnectionDirection> {
        self.connections.get(ip).map(|e| e.direction)
    }

    /// Remove IP when connection ends (outbound)
    pub fn mark_disconnected(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            match state.direction {
                ConnectionDirection::Inbound => {
                    self.inbound_count.fetch_sub(1, Ordering::Relaxed);
                }
                ConnectionDirection::Outbound => {
                    self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Force remove connection (used when accepting inbound over outbound)
    pub fn remove(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            match state.direction {
                ConnectionDirection::Inbound => {
                    self.inbound_count.fetch_sub(1, Ordering::Relaxed);
                }
                ConnectionDirection::Outbound => {
                    self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Remove IP when inbound connection ends
    pub fn mark_inbound_disconnected(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            if state.direction == ConnectionDirection::Inbound {
                self.inbound_count.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Get count of connected peers (both directions) - O(1) with atomics
    pub fn connected_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed) + self.outbound_count.load(Ordering::Relaxed)
    }

    /// Get inbound connection count
    #[allow(dead_code)]
    pub fn inbound_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed)
    }

    /// Get outbound connection count
    #[allow(dead_code)]
    pub fn outbound_count(&self) -> usize {
        self.outbound_count.load(Ordering::Relaxed)
    }

    /// Mark that a peer is in reconnection backoff
    /// This prevents duplicate connection attempts during backoff period
    #[allow(dead_code)]
    pub fn mark_reconnecting(&self, ip: &str, retry_delay: u64, attempt_count: u64) {
        self.reconnecting.insert(
            ip.to_string(),
            ReconnectionState {
                next_attempt: Instant::now() + std::time::Duration::from_secs(retry_delay),
                attempt_count,
            },
        );
    }

    /// Check if a peer is in reconnection backoff
    /// Returns true if we should skip connecting (still in backoff)
    pub fn is_reconnecting(&self, ip: &str) -> bool {
        if let Some(state) = self.reconnecting.get(ip) {
            // Check if backoff period has elapsed
            Instant::now() < state.next_attempt
        } else {
            false
        }
    }

    /// Clear reconnection state when connection succeeds or is abandoned
    pub fn clear_reconnecting(&self, ip: &str) {
        self.reconnecting.remove(ip);
    }

    /// Cleanup stale reconnection states (call periodically)
    #[allow(dead_code)]
    pub fn cleanup_reconnecting(&self, max_age: std::time::Duration) {
        let now = Instant::now();
        self.reconnecting.retain(|_, state| {
            now < state.next_attempt || now.duration_since(state.next_attempt) < max_age
        });
    }
}
