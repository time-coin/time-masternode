use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, RwLock};

use super::message::NetworkMessage;

/// Direction of the connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ConnectionDirection {
    Inbound,  // Peer connected to us
    Outbound, // We connected to peer
}

/// Active connection state for a peer
#[derive(Clone)]
#[allow(dead_code)]
pub struct PeerConnection {
    /// Peer's IP address (unique identifier)
    pub ip: String,

    /// Remote socket address (IP + ephemeral port for inbound, or listening port for outbound)
    pub remote_addr: SocketAddr,

    /// Connection direction
    pub direction: ConnectionDirection,

    /// Channel to send messages to this peer
    pub tx: mpsc::UnboundedSender<NetworkMessage>,

    /// When this connection was established
    pub connected_at: Instant,

    /// Last successful ping/pong time
    pub last_activity: Arc<RwLock<Instant>>,

    /// Missed ping count
    pub missed_pings: Arc<RwLock<u32>>,
}

#[allow(dead_code)]
impl PeerConnection {
    pub fn new(
        ip: String,
        remote_addr: SocketAddr,
        direction: ConnectionDirection,
        tx: mpsc::UnboundedSender<NetworkMessage>,
    ) -> Self {
        let now = Instant::now();
        Self {
            ip,
            remote_addr,
            direction,
            tx,
            connected_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            missed_pings: Arc::new(RwLock::new(0)),
        }
    }

    /// Update last activity timestamp
    pub async fn mark_active(&self) {
        let mut last = self.last_activity.write().await;
        *last = Instant::now();

        // Reset missed pings on activity
        let mut missed = self.missed_pings.write().await;
        *missed = 0;
    }

    /// Increment missed ping counter
    pub async fn increment_missed_pings(&self) -> u32 {
        let mut missed = self.missed_pings.write().await;
        *missed += 1;
        *missed
    }

    /// Get time since last activity
    pub async fn idle_duration(&self) -> std::time::Duration {
        let last = self.last_activity.read().await;
        Instant::now().duration_since(*last)
    }

    /// Send a message to this peer
    pub fn send(&self, message: NetworkMessage) -> Result<(), String> {
        self.tx
            .send(message)
            .map_err(|e| format!("Failed to send message: {}", e))
    }
}

/// Manages all active peer connections
#[allow(dead_code)]
pub struct PeerStateManager {
    /// Active connections by IP address (only one connection per IP)
    connections: Arc<RwLock<HashMap<String, PeerConnection>>>,
}

#[allow(dead_code)]
impl PeerStateManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new connection
    /// Returns Ok(true) if added, Ok(false) if IP already has a connection
    pub async fn add_connection(
        &self,
        ip: String,
        remote_addr: SocketAddr,
        direction: ConnectionDirection,
        tx: mpsc::UnboundedSender<NetworkMessage>,
    ) -> Result<bool, String> {
        let mut conns = self.connections.write().await;

        // Check if already connected
        if conns.contains_key(&ip) {
            return Ok(false);
        }

        let conn = PeerConnection::new(ip.clone(), remote_addr, direction, tx);
        conns.insert(ip, conn);
        Ok(true)
    }

    /// Remove a connection
    pub async fn remove_connection(&self, ip: &str) -> Option<PeerConnection> {
        let mut conns = self.connections.write().await;
        conns.remove(ip)
    }

    /// Get a connection by IP
    pub async fn get_connection(&self, ip: &str) -> Option<PeerConnection> {
        let conns = self.connections.read().await;
        conns.get(ip).cloned()
    }

    /// Check if IP has an active connection
    pub async fn has_connection(&self, ip: &str) -> bool {
        let conns = self.connections.read().await;
        conns.contains_key(ip)
    }

    /// Get count of active connections
    pub async fn connection_count(&self) -> usize {
        let conns = self.connections.read().await;
        conns.len()
    }

    /// Get all connected IPs
    pub async fn get_all_ips(&self) -> Vec<String> {
        let conns = self.connections.read().await;
        conns.keys().cloned().collect()
    }

    /// Get all connections
    pub async fn get_all_connections(&self) -> Vec<PeerConnection> {
        let conns = self.connections.read().await;
        conns.values().cloned().collect()
    }

    /// Broadcast message to all connected peers
    pub async fn broadcast(&self, message: NetworkMessage) -> usize {
        let conns = self.connections.read().await;
        let mut sent = 0;
        for conn in conns.values() {
            if conn.send(message.clone()).is_ok() {
                sent += 1;
            }
        }
        sent
    }

    /// Send message to specific peer
    pub async fn send_to_peer(&self, ip: &str, message: NetworkMessage) -> Result<(), String> {
        let conns = self.connections.read().await;
        if let Some(conn) = conns.get(ip) {
            conn.send(message)
        } else {
            Err(format!("No connection to peer {}", ip))
        }
    }

    /// Update activity timestamp for a peer
    pub async fn mark_peer_active(&self, ip: &str) {
        let conns = self.connections.read().await;
        if let Some(conn) = conns.get(ip) {
            conn.mark_active().await;
        }
    }

    /// Increment missed pings for a peer
    pub async fn increment_missed_pings(&self, ip: &str) -> Option<u32> {
        let conns = self.connections.read().await;
        if let Some(conn) = conns.get(ip) {
            Some(conn.increment_missed_pings().await)
        } else {
            None
        }
    }

    /// Get idle connections (no activity for specified duration)
    pub async fn get_idle_connections(
        &self,
        idle_threshold: std::time::Duration,
    ) -> Vec<PeerConnection> {
        let conns = self.connections.read().await;
        let mut idle = Vec::new();
        for conn in conns.values() {
            if conn.idle_duration().await > idle_threshold {
                idle.push(conn.clone());
            }
        }
        idle
    }
}

impl Default for PeerStateManager {
    fn default() -> Self {
        Self::new()
    }
}
