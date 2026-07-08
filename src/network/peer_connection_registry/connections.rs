use super::types::{ConnectionState, ReconnectionState};
use super::PeerConnectionRegistry;
use crate::network::connection_direction::ConnectionDirection;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

impl PeerConnectionRegistry {
    // ===== Connection Direction Logic =====

    pub fn set_local_ip(&self, ip: String) {
        let _ = self.local_ip.set(ip); // ignore if already set
    }

    pub fn get_local_ip(&self) -> Option<String> {
        self.local_ip.get().cloned()
    }

    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        if let Some(local_ip) = self.local_ip.get() {
            if let (Ok(local_addr), Ok(peer_addr)) =
                (local_ip.parse::<IpAddr>(), peer_ip.parse::<IpAddr>())
            {
                match (local_addr, peer_addr) {
                    (IpAddr::V4(l), IpAddr::V4(p)) => l.octets() > p.octets(),
                    (IpAddr::V6(l), IpAddr::V6(p)) => l.octets() > p.octets(),
                    (IpAddr::V6(_), IpAddr::V4(_)) => true,
                    (IpAddr::V4(_), IpAddr::V6(_)) => false,
                }
            } else {
                local_ip.as_str() > peer_ip
            }
        } else {
            true
        }
    }

    // ===== Connection State Management =====

    /// Atomically register inbound connection if not already connected
    /// Returns true if registration succeeded, false if already exists
    /// This prevents race conditions during concurrent connection attempts.
    /// Uses IP-based tiebreaker for simultaneous connections: the node with
    /// the lower IP keeps its outbound; the higher IP yields and accepts inbound.
    pub fn try_register_inbound(&self, ip: &str) -> bool {
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
            Entry::Occupied(mut e) => {
                if e.get().direction == ConnectionDirection::Outbound {
                    // Simultaneous connection: we have outbound, peer is sending inbound.
                    // Use deterministic tiebreaker: node with higher IP yields its outbound
                    // and accepts inbound instead. This prevents the reconnect loop where
                    // rejecting the inbound causes the peer to close our outbound too.
                    if !self.should_connect_to(ip) {
                        // Our IP is lower — we should yield outbound, accept inbound
                        tracing::info!(
                            "🔄 Simultaneous connection with {} — yielding outbound, accepting inbound (IP tiebreaker)",
                            ip
                        );
                        self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                        e.insert(ConnectionState {
                            direction: ConnectionDirection::Inbound,
                            connected_at: Instant::now(),
                        });
                        self.inbound_count.fetch_add(1, Ordering::Relaxed);
                        true
                    } else {
                        // Our IP is higher — keep outbound, reject inbound
                        tracing::debug!(
                            "🔄 Rejecting inbound from {} - outbound connection preferred (IP tiebreaker)",
                            ip
                        );
                        false
                    }
                } else {
                    // Already have an inbound connection, reject duplicate
                    tracing::debug!(
                        "🔄 Rejecting duplicate inbound from {} - already connected",
                        ip
                    );
                    false
                }
            }
        }
    }

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
            Entry::Occupied(_) => {
                // Reject if any connection (inbound or outbound) already exists.
                // Prevents outbound from racing with an active inbound connection
                // and corrupting the writer channel.
                tracing::debug!(
                    "🔄 Rejecting outbound to {} - connection already exists",
                    ip
                );
                false
            }
        }
    }

    pub fn is_connected(&self, ip: &str) -> bool {
        self.connections.contains_key(ip)
    }

    /// Check if the current connection to a peer is outbound.
    /// Returns false if not connected or if the connection is inbound.
    pub fn is_outbound(&self, ip: &str) -> bool {
        self.connections
            .get(ip)
            .map(|s| s.direction == ConnectionDirection::Outbound)
            .unwrap_or(false)
    }

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
            Entry::Occupied(mut e) => {
                // Allow reconnection by updating existing entry
                let old_direction = e.get().direction;
                e.insert(ConnectionState {
                    direction: ConnectionDirection::Inbound,
                    connected_at: Instant::now(),
                });
                // Adjust counters if direction changed
                if old_direction == ConnectionDirection::Outbound {
                    self.outbound_count.fetch_sub(1, Ordering::Relaxed);
                    self.inbound_count.fetch_add(1, Ordering::Relaxed);
                }
                true
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_direction(&self, ip: &str) -> Option<ConnectionDirection> {
        self.connections.get(ip).map(|e| e.direction)
    }

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
            // Clean up stale peer metadata
            tokio::spawn({
                let peer_chain_tips = Arc::clone(&self.peer_chain_tips);
                let peer_heights = Arc::clone(&self.peer_heights);
                let peer_ping_times = Arc::clone(&self.peer_ping_times);
                let pending_pings = Arc::clone(&self.pending_pings);
                let peer_commit_counts = Arc::clone(&self.peer_commit_counts);
                let ip = ip.to_string();
                async move {
                    // Drop each write lock before acquiring the next to avoid
                    // holding multiple locks across await points.
                    {
                        peer_chain_tips.write().await.remove(&ip);
                    }
                    {
                        peer_heights.write().await.remove(&ip);
                    }
                    {
                        peer_ping_times.write().await.remove(&ip);
                    }
                    {
                        pending_pings.write().await.remove(&ip);
                    }
                    {
                        peer_commit_counts.write().await.remove(&ip);
                    }
                }
            });
        }
    }

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
            // Clean up stale peer metadata
            tokio::spawn({
                let peer_chain_tips = Arc::clone(&self.peer_chain_tips);
                let peer_heights = Arc::clone(&self.peer_heights);
                let peer_ping_times = Arc::clone(&self.peer_ping_times);
                let pending_pings = Arc::clone(&self.pending_pings);
                let peer_commit_counts = Arc::clone(&self.peer_commit_counts);
                let ip = ip.to_string();
                async move {
                    {
                        peer_chain_tips.write().await.remove(&ip);
                    }
                    {
                        peer_heights.write().await.remove(&ip);
                    }
                    {
                        peer_ping_times.write().await.remove(&ip);
                    }
                    {
                        pending_pings.write().await.remove(&ip);
                    }
                    {
                        peer_commit_counts.write().await.remove(&ip);
                    }
                }
            });
        }
    }

    pub fn mark_inbound_disconnected(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            if state.direction == ConnectionDirection::Inbound {
                self.inbound_count.fetch_sub(1, Ordering::Relaxed);
            }
            // Clean up stale peer metadata
            tokio::spawn({
                let peer_chain_tips = Arc::clone(&self.peer_chain_tips);
                let peer_heights = Arc::clone(&self.peer_heights);
                let peer_ping_times = Arc::clone(&self.peer_ping_times);
                let pending_pings = Arc::clone(&self.pending_pings);
                let peer_commit_counts = Arc::clone(&self.peer_commit_counts);
                let ip = ip.to_string();
                async move {
                    {
                        peer_chain_tips.write().await.remove(&ip);
                    }
                    {
                        peer_heights.write().await.remove(&ip);
                    }
                    {
                        peer_ping_times.write().await.remove(&ip);
                    }
                    {
                        pending_pings.write().await.remove(&ip);
                    }
                    {
                        peer_commit_counts.write().await.remove(&ip);
                    }
                }
            });
        }
    }

    pub fn connected_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed) + self.outbound_count.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn inbound_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn outbound_count(&self) -> usize {
        self.outbound_count.load(Ordering::Relaxed)
    }

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

    pub fn is_reconnecting(&self, ip: &str) -> bool {
        if let Some(state) = self.reconnecting.get(ip) {
            Instant::now() < state.next_attempt
        } else {
            false
        }
    }

    pub fn clear_reconnecting(&self, ip: &str) {
        self.reconnecting.remove(ip);
    }

    #[allow(dead_code)]
    pub fn cleanup_reconnecting(&self, max_age: std::time::Duration) {
        let now = Instant::now();
        self.reconnecting.retain(|_, state| {
            now < state.next_attempt || now.duration_since(state.next_attempt) < max_age
        });
    }
}
