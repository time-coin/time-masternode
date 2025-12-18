use crate::network::message::NetworkMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, warn};

type PeerWriter = BufWriter<OwnedWriteHalf>;
type ResponseSender = oneshot::Sender<NetworkMessage>;

/// Extract IP address from "IP:PORT" or just "IP" strings
fn extract_ip(addr: &str) -> &str {
    addr.split(':').next().unwrap_or(addr)
}

/// Registry of active peer connections with ability to send targeted messages
/// Note: Infrastructure for Phase 2 of PeerConnectionRegistry integration
/// See analysis/TODO_PeerConnectionRegistry_Integration.md
#[allow(dead_code)]
pub struct PeerConnectionRegistry {
    /// Map of peer IP to their TCP writer
    connections: Arc<RwLock<HashMap<String, PeerWriter>>>,
    /// Pending responses for request/response pattern
    pending_responses: Arc<RwLock<HashMap<String, Vec<ResponseSender>>>>,
}

#[allow(dead_code)]
impl PeerConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a peer connection
    pub async fn register_peer(&self, peer_ip: String, writer: PeerWriter) {
        let mut connections = self.connections.write().await;
        connections.insert(peer_ip.clone(), writer);
        debug!("✅ Registered peer connection: {}", peer_ip);
    }

    /// Unregister a peer connection
    pub async fn unregister_peer(&self, peer_ip: &str) {
        let mut connections = self.connections.write().await;
        connections.remove(peer_ip);
        debug!("🔌 Unregistered peer connection: {}", peer_ip);

        // Clean up any pending responses for this peer
        let mut pending = self.pending_responses.write().await;
        pending.remove(peer_ip);
    }

    /// Send a message to a specific peer (optimized, minimal logging)
    pub async fn send_to_peer(&self, peer_ip: &str, message: NetworkMessage) -> Result<(), String> {
        // Extract IP only (remove port if present)
        let ip_only = extract_ip(peer_ip);

        let mut connections = self.connections.write().await;

        if let Some(writer) = connections.get_mut(ip_only) {
            let msg_json = serde_json::to_string(&message)
                .map_err(|e| format!("Failed to serialize message: {}", e))?;

            writer
                .write_all(format!("{}\n", msg_json).as_bytes())
                .await
                .map_err(|e| format!("Failed to write to peer {}: {}", ip_only, e))?;

            writer
                .flush()
                .await
                .map_err(|e| format!("Failed to flush to peer {}: {}", ip_only, e))?;

            Ok(())
        } else {
            warn!(
                "❌ Peer {} not found in registry (available: {:?})",
                ip_only,
                connections.keys().collect::<Vec<_>>()
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
        // Serialize once instead of per-peer
        let msg_json = match serde_json::to_string(&message) {
            Ok(json) => json,
            Err(e) => {
                warn!("Failed to serialize broadcast message: {}", e);
                return;
            }
        };

        let msg_bytes = format!("{}\n", msg_json);
        let mut connections = self.connections.write().await;
        let mut disconnected_peers = Vec::new();

        for (peer_ip, writer) in connections.iter_mut() {
            // Write pre-serialized bytes to avoid redundant serialization
            if let Err(e) = writer.write_all(msg_bytes.as_bytes()).await {
                warn!("Failed to broadcast to {}: {}", peer_ip, e);
                disconnected_peers.push(peer_ip.clone());
                continue;
            }

            if let Err(e) = writer.flush().await {
                warn!("Failed to flush broadcast to {}: {}", peer_ip, e);
                disconnected_peers.push(peer_ip.clone());
            }
        }

        // Remove disconnected peers
        for peer_ip in disconnected_peers {
            connections.remove(&peer_ip);
        }
    }

    /// Get list of connected peer IPs
    pub async fn get_connected_peers(&self) -> Vec<String> {
        let connections = self.connections.read().await;
        connections.keys().cloned().collect()
    }

    /// Get count of connected peers
    pub async fn peer_count(&self) -> usize {
        let connections = self.connections.read().await;
        connections.len()
    }

    /// Get a snapshot of connected peer IPs (for stats/monitoring)
    pub async fn get_connected_peers_list(&self) -> Vec<String> {
        let connections = self.connections.read().await;
        connections.keys().cloned().collect()
    }

    /// Get statistics about pending responses (for monitoring)
    pub async fn pending_response_count(&self) -> usize {
        let pending = self.pending_responses.read().await;
        pending.values().map(|senders| senders.len()).sum()
    }

    /// Send multiple messages to a peer in a batch (more efficient than individual sends)
    pub async fn send_batch_to_peer(&self, peer_ip: &str, messages: &[NetworkMessage]) -> Result<(), String> {
        if messages.is_empty() {
            return Ok(());
        }

        let ip_only = extract_ip(peer_ip);
        let mut connections = self.connections.write().await;

        if let Some(writer) = connections.get_mut(ip_only) {
            // Serialize all messages and batch writes
            for message in messages {
                let msg_json = serde_json::to_string(message)
                    .map_err(|e| format!("Failed to serialize message: {}", e))?;

                writer
                    .write_all(format!("{}\n", msg_json).as_bytes())
                    .await
                    .map_err(|e| format!("Failed to write to peer {}: {}", ip_only, e))?;
            }

            // Single flush for all messages
            writer
                .flush()
                .await
                .map_err(|e| format!("Failed to flush to peer {}: {}", ip_only, e))?;

            Ok(())
        } else {
            warn!("❌ Peer {} not found in registry", ip_only);
            Err(format!("Peer {} not connected", ip_only))
        }
    }

    /// Broadcast multiple messages to all connected peers efficiently
    pub async fn broadcast_batch(&self, messages: &[NetworkMessage]) {
        if messages.is_empty() {
            return;
        }

        // Pre-serialize all messages once
        let serialized: Vec<String> = messages
            .iter()
            .filter_map(|msg| {
                serde_json::to_string(msg)
                    .ok()
                    .map(|json| format!("{}\n", json))
            })
            .collect();

        if serialized.is_empty() {
            warn!("Failed to serialize broadcast batch messages");
            return;
        }

        let combined = serialized.join("");
        let mut connections = self.connections.write().await;
        let mut disconnected_peers = Vec::new();

        for (peer_ip, writer) in connections.iter_mut() {
            if let Err(e) = writer.write_all(combined.as_bytes()).await {
                warn!("Failed to broadcast batch to {}: {}", peer_ip, e);
                disconnected_peers.push(peer_ip.clone());
                continue;
            }

            if let Err(e) = writer.flush().await {
                warn!("Failed to flush broadcast batch to {}: {}", peer_ip, e);
                disconnected_peers.push(peer_ip.clone());
            }
        }

        // Remove disconnected peers
        for peer_ip in disconnected_peers {
            connections.remove(&peer_ip);
        }
    }
}

impl Default for PeerConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
