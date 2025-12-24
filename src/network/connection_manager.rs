//! Connection manager for tracking peer connection state
//! Uses DashMap for lock-free concurrent access to connection states

#![allow(dead_code)]

use dashmap::DashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// Manages the lifecycle of peer connections (inbound/outbound)
pub struct ConnectionManager {
    states: Arc<DashMap<String, PeerConnectionState>>,
    connected_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl ConnectionManager {
    /// Create a new connection manager
    pub fn new() -> Self {
        Self {
            states: Arc::new(DashMap::new()),
            connected_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Check if we're already connected to a peer
    pub fn is_connected(&self, peer_ip: &str) -> bool {
        self.states
            .get(peer_ip)
            .map(|state| *state == PeerConnectionState::Connected)
            .unwrap_or(false)
    }

    /// Check if we should connect to a peer
    /// Returns false if already connected or currently connecting
    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        !self.states.contains_key(peer_ip)
            || self
                .states
                .get(peer_ip)
                .map(|state| *state == PeerConnectionState::Disconnected)
                .unwrap_or(false)
    }

    /// Mark a peer as connected (inbound connection)
    pub fn mark_inbound(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.states.get_mut(peer_ip) {
            if *entry == PeerConnectionState::Connecting {
                *entry = PeerConnectionState::Connected;
                self.connected_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                true
            } else {
                false
            }
        } else {
            self.states
                .insert(peer_ip.to_string(), PeerConnectionState::Connected);
            self.connected_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        }
    }

    /// Mark an inbound connection as disconnected
    pub fn mark_inbound_disconnected(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.states.get_mut(peer_ip) {
            if *entry == PeerConnectionState::Connected {
                *entry = PeerConnectionState::Disconnected;
                self.connected_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    /// Mark a peer as being attempted for connection
    pub fn mark_connecting(&self, peer_ip: &str) -> bool {
        if self.states.contains_key(peer_ip) {
            false
        } else {
            self.states
                .insert(peer_ip.to_string(), PeerConnectionState::Connecting);
            true
        }
    }

    /// Mark a connection attempt as successfully connected
    pub fn mark_connected(&self, peer_ip: &str) -> bool {
        if let Some(mut entry) = self.states.get_mut(peer_ip) {
            if *entry == PeerConnectionState::Connecting {
                *entry = PeerConnectionState::Connected;
                self.connected_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        if let Some(mut entry) = self.states.get_mut(peer_ip) {
            *entry = PeerConnectionState::Reconnecting;
            true
        } else {
            self.states
                .insert(peer_ip.to_string(), PeerConnectionState::Reconnecting);
            true
        }
    }

    /// Remove a peer from tracking (cleanup)
    pub fn remove(&self, peer_ip: &str) {
        if let Some((_, state)) = self.states.remove(peer_ip) {
            if state == PeerConnectionState::Connected {
                self.connected_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// Get count of connected peers
    pub fn connected_count(&self) -> usize {
        self.connected_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Check if a peer is in reconnecting state
    pub fn is_reconnecting(&self, peer_ip: &str) -> bool {
        self.states
            .get(peer_ip)
            .map(|state| *state == PeerConnectionState::Reconnecting)
            .unwrap_or(false)
    }

    /// Mark a peer as disconnected
    pub fn mark_disconnected(&self, peer_ip: &str) {
        if let Some(mut entry) = self.states.get_mut(peer_ip) {
            if *entry == PeerConnectionState::Connected {
                self.connected_count
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
            *entry = PeerConnectionState::Disconnected;
        }
    }

    /// Clear reconnecting state for a peer (allow immediate retry)
    pub fn clear_reconnecting(&self, peer_ip: &str) {
        if let Some(mut entry) = self.states.get_mut(peer_ip) {
            if *entry == PeerConnectionState::Reconnecting {
                *entry = PeerConnectionState::Disconnected;
            }
        }
    }

    /// Mark a peer as reconnecting (with retry logic)
    pub fn mark_reconnecting(
        &self,
        _peer_ip: &str,
        _retry_delay: std::time::Duration,
        _consecutive_failures: u32,
    ) {
        // Reconnection tracking with exponential backoff
        // For now, just a placeholder
    }

    /// Get list of currently connected peers
    pub fn get_connected_peers(&self) -> Vec<String> {
        self.states
            .iter()
            .filter(|entry| *entry.value() == PeerConnectionState::Connected)
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get list of peers currently being connected to
    pub fn get_connecting_peers(&self) -> Vec<String> {
        self.states
            .iter()
            .filter(|entry| *entry.value() == PeerConnectionState::Connecting)
            .map(|entry| entry.key().clone())
            .collect()
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}
