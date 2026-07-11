use super::PeerConnectionRegistry;
use crate::network::connection_direction::ConnectionDirection;
use crate::network::connection_manager::ConnectionManager;
use std::sync::Arc;

impl PeerConnectionRegistry {
    /// Wire the shared ConnectionManager (called once at daemon startup).
    pub fn set_connection_manager(&self, manager: Arc<ConnectionManager>) {
        let _ = self.connection_manager.set(manager);
    }

    pub(super) fn connection_manager(&self) -> Option<&Arc<ConnectionManager>> {
        self.connection_manager.get()
    }

    pub fn set_local_ip(&self, ip: String) {
        if let Some(cm) = self.connection_manager() {
            cm.set_local_ip(ip);
        }
    }

    pub fn get_local_ip(&self) -> Option<String> {
        self.connection_manager().and_then(|cm| cm.get_local_ip())
    }

    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        self.connection_manager()
            .map(|cm| cm.should_connect_to(peer_ip))
            .unwrap_or(true)
    }

    /// Higher-IP-dials-lower: whether we should initiate outbound to this peer.
    pub fn is_preferred_dialer(&self, peer_ip: &str) -> bool {
        self.connection_manager()
            .map(|cm| cm.is_preferred_dialer(peer_ip))
            .unwrap_or(true)
    }

    pub fn is_connected(&self, ip: &str) -> bool {
        if let Some(cm) = self.connection_manager() {
            return cm.is_connected(ip);
        }
        false
    }

    pub fn is_outbound(&self, ip: &str) -> bool {
        self.connection_manager()
            .map(|cm| cm.has_outbound_connection(ip))
            .unwrap_or(false)
    }

    pub fn get_direction(&self, ip: &str) -> Option<ConnectionDirection> {
        self.connection_manager()
            .and_then(|cm| cm.connection_direction(ip))
    }

    /// Direction only for a fully Connected CM session.
    pub fn direction_if_connected(&self, ip: &str) -> Option<ConnectionDirection> {
        self.connection_manager()
            .and_then(|cm| cm.direction_if_connected(ip))
    }

    /// Number of currently connected peers.
    ///
    /// Uses post-handshake writer channels as the source of truth (same basis as
    /// [`PeerConnectionRegistry::peer_count`]). ConnectionManager state can lag or
    /// desync under connection races; under-reporting as 0 previously caused
    /// false-positive zero-peer watchdog restarts while message loops were live.
    ///
    /// Falls back to ConnectionManager only if the writer lock is busy.
    pub fn connected_count(&self) -> usize {
        if let Ok(writers) = self.peer_writers.try_read() {
            return writers.values().filter(|w| !w.is_closed()).count();
        }
        self.connection_manager()
            .map(|cm| cm.connected_count())
            .unwrap_or(0)
    }

    /// CM-only inbound count (may include zombie Connected slots without writers).
    pub fn inbound_count(&self) -> usize {
        self.connection_manager()
            .map(|cm| cm.inbound_count())
            .unwrap_or(0)
    }

    /// CM-only outbound count (may include zombie Connected slots without writers).
    pub fn outbound_count(&self) -> usize {
        self.connection_manager()
            .map(|cm| cm.outbound_count())
            .unwrap_or(0)
    }

    /// Live TCP direction counts: only peers with a non-closed writer channel.
    /// Returns `(total, inbound, outbound)` so `inbound + outbound == total`.
    pub async fn live_direction_counts(&self) -> (usize, usize, usize) {
        let writers = self.peer_writers.read().await;
        let mut inbound = 0usize;
        let mut outbound = 0usize;
        for (ip, w) in writers.iter() {
            if w.is_closed() {
                continue;
            }
            match self.direction_if_connected(ip) {
                Some(ConnectionDirection::Inbound) => inbound += 1,
                Some(ConnectionDirection::Outbound) => outbound += 1,
                // Writer live but CM missing/stale — still a real session; don't
                // drop it from totals. Treat as outbound for the in/out split.
                None => outbound += 1,
            }
        }
        (inbound + outbound, inbound, outbound)
    }

    /// True if this IP has a non-closed post-handshake writer (live TCP session).
    pub async fn has_live_writer(&self, ip: &str) -> bool {
        let ip_only = super::types::extract_ip(ip);
        let writers = self.peer_writers.read().await;
        writers
            .get(ip_only)
            .map(|w| !w.is_closed())
            .unwrap_or(false)
    }

    /// Sync variant for dial-skip helpers (try_read; false if lock is busy).
    pub fn has_live_writer_sync(&self, ip: &str) -> bool {
        let ip_only = super::types::extract_ip(ip);
        self.peer_writers
            .try_read()
            .map(|writers| {
                writers
                    .get(ip_only)
                    .map(|w| !w.is_closed())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    pub fn is_reconnecting(&self, ip: &str) -> bool {
        self.connection_manager()
            .map(|cm| cm.is_reconnecting(ip))
            .unwrap_or(false)
    }

    pub fn clear_reconnecting(&self, ip: &str) {
        if let Some(cm) = self.connection_manager() {
            cm.clear_reconnecting(ip);
        }
    }
}
