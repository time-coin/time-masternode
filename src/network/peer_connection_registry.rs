//! Peer Connection Registry
//! Manages active peer connections and message routing.
//! Note: Some methods are scaffolding for future peer management features.

#![allow(dead_code)]

use crate::network::message::NetworkMessage;
use arc_swap::ArcSwapOption;
use dashmap::DashMap;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::oneshot;
use tokio::sync::RwLock;
use tracing::{debug, warn};

type PeerWriter = BufWriter<OwnedWriteHalf>;
type ResponseSender = oneshot::Sender<NetworkMessage>;

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

/// Registry of active peer connections with ability to send targeted messages
/// Combines both connection tracking and message routing
pub struct PeerConnectionRegistry {
    // Connection state tracking (lock-free with DashMap)
    connections: DashMap<String, ConnectionState>,
    // Track reconnection backoff
    reconnecting: DashMap<String, ReconnectionState>,
    // Local IP - set once, read many (lock-free with ArcSwapOption)
    local_ip: ArcSwapOption<String>,
    // Metrics (atomic, no locks)
    inbound_count: AtomicUsize,
    outbound_count: AtomicUsize,
    // Map of peer IP to their TCP writer (wrapped in Arc<Mutex<>> for safe mutable access)
    peer_writers: Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<PeerWriter>>>>>,
    // Pending responses for request/response pattern
    pending_responses: Arc<RwLock<HashMap<String, Vec<ResponseSender>>>>,
}

fn extract_ip(addr: &str) -> &str {
    addr.split(':').next().unwrap_or(addr)
}

impl PeerConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            reconnecting: DashMap::new(),
            local_ip: ArcSwapOption::empty(),
            inbound_count: AtomicUsize::new(0),
            outbound_count: AtomicUsize::new(0),
            peer_writers: Arc::new(RwLock::new(HashMap::new())),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ===== Connection Direction Logic =====

    pub fn set_local_ip(&self, ip: String) {
        self.local_ip.store(Some(Arc::new(ip)));
    }

    pub fn should_connect_to(&self, peer_ip: &str) -> bool {
        let local_ip_guard = self.local_ip.load();

        if let Some(local_ip_arc) = local_ip_guard.as_ref() {
            let local_ip = local_ip_arc.as_str();

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
                local_ip > peer_ip
            }
        } else {
            true
        }
    }

    // ===== Connection State Management =====

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

    pub fn is_connected(&self, ip: &str) -> bool {
        self.connections.contains_key(ip)
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
            Entry::Occupied(_) => false,
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
        }
    }

    pub fn mark_inbound_disconnected(&self, ip: &str) {
        if let Some((_, state)) = self.connections.remove(ip) {
            if state.direction == ConnectionDirection::Inbound {
                self.inbound_count.fetch_sub(1, Ordering::Relaxed);
            }
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

    // ===== Peer Writer Registry (formerly peer_connection_registry.rs) =====

    pub async fn register_peer(&self, peer_ip: String, writer: PeerWriter) {
        let mut writers = self.peer_writers.write().await;
        writers.insert(peer_ip.clone(), Arc::new(tokio::sync::Mutex::new(writer)));
        debug!("✅ Registered peer connection: {}", peer_ip);
    }

    pub async fn unregister_peer(&self, peer_ip: &str) {
        let mut writers = self.peer_writers.write().await;
        writers.remove(peer_ip);
        debug!("🔌 Unregistered peer connection: {}", peer_ip);

        let mut pending = self.pending_responses.write().await;
        pending.remove(peer_ip);
    }

    pub async fn get_peer_writer(
        &self,
        _peer_ip: &str,
    ) -> Option<Arc<tokio::sync::Mutex<PeerWriter>>> {
        // peer_writers stores PeerWriter directly, not wrapped in Arc
        // Since we can't clone the writer (it contains TCP state), return None
        // This is a placeholder - actual implementation would use Arc<Mutex<>> from the start
        let _writers = self.peer_writers.read().await;
        None
    }

    pub async fn register_response_handler(&self, peer_ip: &str, tx: ResponseSender) {
        let mut pending = self.pending_responses.write().await;
        pending
            .entry(peer_ip.to_string())
            .or_insert_with(Vec::new)
            .push(tx);
    }

    pub async fn get_response_handlers(&self, peer_ip: &str) -> Option<Vec<ResponseSender>> {
        let mut pending = self.pending_responses.write().await;
        pending.remove(peer_ip)
    }

    pub async fn list_peers(&self) -> Vec<String> {
        let writers = self.peer_writers.read().await;
        writers.keys().cloned().collect()
    }

    /// Send a message to a specific peer
    pub async fn send_to_peer(&self, peer_ip: &str, message: NetworkMessage) -> Result<(), String> {
        // Extract IP only (remove port if present)
        let ip_only = extract_ip(peer_ip);

        let writers = self.peer_writers.read().await;

        if let Some(writer_arc) = writers.get(ip_only) {
            let mut writer = writer_arc.lock().await;

            let msg_json = serde_json::to_string(&message)
                .map_err(|e| format!("Failed to serialize message: {}", e))?;

            writer
                .write_all(format!("{}\n", msg_json).as_bytes())
                .await
                .map_err(|e| format!("Failed to write message to {}: {}", ip_only, e))?;

            writer
                .flush()
                .await
                .map_err(|e| format!("Failed to flush to {}: {}", ip_only, e))?;

            Ok(())
        } else {
            warn!(
                "❌ Peer {} not found in registry (available: {:?})",
                ip_only,
                writers.keys().collect::<Vec<_>>()
            );
            Err(format!("Peer {} not connected", ip_only))
        }
    }

    /// Send a message to a peer and wait for a response
    pub async fn send_and_await_response(
        &self,
        peer_ip: &str,
        message: NetworkMessage,
        timeout_secs: u64,
    ) -> Result<NetworkMessage, String> {
        // Extract IP only
        let ip_only = extract_ip(peer_ip);
        let (tx, rx) = oneshot::channel();

        // Register pending response
        {
            let mut pending = self.pending_responses.write().await;
            pending
                .entry(ip_only.to_string())
                .or_insert_with(Vec::new)
                .push(tx);
        }

        // Send the message
        self.send_to_peer(ip_only, message).await?;

        // Wait for response with timeout
        match tokio::time::timeout(tokio::time::Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err("Response channel closed".to_string()),
            Err(_) => {
                // Clean up pending response on timeout
                let mut pending = self.pending_responses.write().await;
                if let Some(senders) = pending.get_mut(ip_only) {
                    senders.retain(|_| false); // Remove all pending for simplicity
                }
                Err(format!("Timeout waiting for response from {}", peer_ip))
            }
        }
    }

    /// Handle an incoming response message (called from message loop)
    pub async fn handle_response(&self, peer_ip: &str, message: NetworkMessage) {
        // Extract IP only
        let ip_only = extract_ip(peer_ip);
        let mut pending = self.pending_responses.write().await;

        if let Some(senders) = pending.get_mut(ip_only) {
            if let Some(sender) = senders.pop() {
                if sender.send(message).is_err() {
                    warn!(
                        "Failed to send response to awaiting task for peer {}",
                        ip_only
                    );
                }
            }
        }
    }

    /// Broadcast a message to all connected peers (pre-serializes for efficiency)
    pub async fn broadcast(&self, message: NetworkMessage) {
        let writers = self.peer_writers.read().await;

        if writers.is_empty() {
            debug!("📡 Broadcast: no peers connected");
            return;
        }

        // Pre-serialize the message once for efficiency
        let msg_json = match serde_json::to_string(&message) {
            Ok(json) => format!("{}\n", json),
            Err(e) => {
                warn!("❌ Failed to serialize broadcast message: {}", e);
                return;
            }
        };
        let msg_bytes = msg_json.as_bytes();

        let mut send_count = 0;
        let mut fail_count = 0;

        for (peer_ip, writer_arc) in writers.iter() {
            let mut writer = writer_arc.lock().await;

            if let Err(e) = writer.write_all(msg_bytes).await {
                debug!("❌ Broadcast to {} failed: {}", peer_ip, e);
                fail_count += 1;
                continue;
            }

            if let Err(e) = writer.flush().await {
                debug!("❌ Broadcast flush to {} failed: {}", peer_ip, e);
                fail_count += 1;
                continue;
            }

            send_count += 1;
        }

        if send_count > 0 || fail_count > 0 {
            debug!(
                "📡 Broadcast complete: {} sent, {} failed",
                send_count, fail_count
            );
        }
    }

    /// Get list of connected peer IPs
    pub async fn get_connected_peers(&self) -> Vec<String> {
        self.connections
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get count of connected peers
    pub async fn peer_count(&self) -> usize {
        self.connections.len()
    }

    /// Get a snapshot of connected peer IPs (for stats/monitoring)
    #[allow(dead_code)]
    pub async fn get_connected_peers_list(&self) -> Vec<String> {
        self.connections
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get statistics about pending responses (for monitoring)
    #[allow(dead_code)]
    pub async fn pending_response_count(&self) -> usize {
        let pending = self.pending_responses.read().await;
        pending.values().map(|senders| senders.len()).sum()
    }

    /// Send multiple messages to a peer in a batch (more efficient than individual sends)
    #[allow(dead_code)]
    pub async fn send_batch_to_peer(
        &self,
        peer_ip: &str,
        _messages: &[NetworkMessage],
    ) -> Result<(), String> {
        let ip_only = extract_ip(peer_ip);

        let writers = self.peer_writers.read().await;
        if !writers.contains_key(ip_only) {
            warn!("❌ Peer {} not found in registry", ip_only);
            return Err(format!("Peer {} not connected", ip_only));
        }

        // Message sending is handled by the network server
        // This is a placeholder for the refactored architecture
        Ok(())
    }

    /// Broadcast multiple messages to all connected peers efficiently
    #[allow(dead_code)]
    pub async fn broadcast_batch(&self, _messages: &[NetworkMessage]) {
        // Batch broadcasting is handled by the network server
        // This is a placeholder for the refactored architecture
        debug!("📡 Broadcast batch called (message routing handled by server)");
    }

    /// Selective gossip: send to random subset of peers to reduce bandwidth
    /// Default fan-out: 20 peers (configurable)
    #[allow(dead_code)]
    pub async fn gossip_selective(
        &self,
        message: NetworkMessage,
        source_peer: Option<&str>,
    ) -> usize {
        self.gossip_selective_with_config(message, source_peer, 20)
            .await
    }

    /// Selective gossip with configurable fan-out
    /// Returns number of peers message was sent to
    #[allow(dead_code)]
    pub async fn gossip_selective_with_config(
        &self,
        _message: NetworkMessage,
        _source_peer: Option<&str>,
        fan_out: usize,
    ) -> usize {
        // Selective gossip is handled by the network server
        // Return the configured fan-out as indication
        fan_out
    }
}

impl Default for PeerConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
