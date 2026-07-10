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

    pub fn inbound_count(&self) -> usize {
        self.connection_manager()
            .map(|cm| cm.inbound_count())
            .unwrap_or(0)
    }

    pub fn outbound_count(&self) -> usize {
        self.connection_manager()
            .map(|cm| cm.outbound_count())
            .unwrap_or(0)
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
