use super::types::{extract_ip, PeerWriterTx, ResponseSender, SharedPeerWriter};
use super::PeerConnectionRegistry;
use crate::network::message::NetworkMessage;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

impl PeerConnectionRegistry {
    // ===== Peer Writer Registry (formerly peer_connection_registry.rs) =====

    pub async fn register_peer(&self, peer_ip: String, writer: PeerWriterTx) {
        // Inbound handshake always owns the writer slot. A live outbound (or
        // stale inbound) writer must be replaced so dual-connect yield cannot
        // leave the registry pointing at a superseded channel.
        self.register_peer_force(peer_ip, writer).await;
    }

    /// Always install `writer` for `peer_ip`, dropping any previous sender.
    /// Dropping the old sender collapses the prior write loop and forces that
    /// session to exit.
    pub async fn register_peer_force(&self, peer_ip: String, writer: PeerWriterTx) {
        let mut writers = self.peer_writers.write().await;
        if let Some(existing) = writers.insert(peer_ip.clone(), writer) {
            if !existing.is_closed() {
                info!(
                    "♻️ Replaced live writer for peer {} (superseded session will exit)",
                    peer_ip
                );
            } else {
                debug!("♻️ Replacing dead writer for peer {}", peer_ip);
            }
        } else {
            debug!("✅ Registered peer connection: {}", peer_ip);
        }
    }

    /// Drop any live writer for `peer_ip` without clearing heights/ping maps.
    /// Used immediately after ConnectionManager yields/replaces so the old
    /// outbound (or replaced inbound) I/O task begins shutting down before the
    /// new session finishes handshake.
    pub async fn evict_writer(&self, peer_ip: &str) {
        let mut writers = self.peer_writers.write().await;
        if writers.remove(peer_ip).is_some() {
            info!(
                "🔌 Evicted writer for {} so superseded connection can tear down",
                peer_ip
            );
        }
    }

    /// Register an outbound peer with a channel-based writer
    pub async fn register_peer_shared(&self, peer_ip: String, writer: SharedPeerWriter) {
        let mut writers = self.peer_writers.write().await;
        // Only overwrite if the existing writer is dead (channel closed).
        // A live writer (e.g., from an accepted inbound connection) must not be
        // replaced by a speculative outbound writer that may be rejected.
        if let Some(existing) = writers.get(&peer_ip) {
            if !existing.is_closed() {
                debug!(
                    "🔄 Outbound peer {} already has a live writer, skipping overwrite",
                    peer_ip
                );
                return;
            }
            debug!("♻️ Replacing dead writer for peer {}", peer_ip);
        }
        writers.insert(peer_ip.clone(), writer);
        debug!("✅ Registered outbound peer connection: {}", peer_ip);
    }

    pub async fn unregister_peer(&self, peer_ip: &str) {
        // Drop each write lock before acquiring the next to avoid holding
        // multiple locks across await points.  The old code held all 6 write
        // locks simultaneously (variables lived until end-of-scope), which
        // blocked ALL broadcast/send_to_peer calls for the entire duration.
        {
            self.peer_writers.write().await.remove(peer_ip);
        }
        debug!("🔌 Unregistered peer connection: {}", peer_ip);
        {
            self.pending_responses.write().await.remove(peer_ip);
        }
        {
            self.peer_heights.write().await.remove(peer_ip);
        }
        {
            self.peer_ping_times.write().await.remove(peer_ip);
        }
        {
            self.pending_pings.write().await.remove(peer_ip);
        }
        {
            self.peer_commit_counts.write().await.remove(peer_ip);
        }
    }
    pub async fn get_peer_writer(&self, peer_ip: &str) -> Option<PeerWriterTx> {
        let ip_only = extract_ip(peer_ip);
        let writers = self.peer_writers.read().await;
        writers.get(ip_only).cloned()
    }

    /// Forcibly close the write channel for a peer.
    ///
    /// Drops the sender half of the peer's write channel, causing the write
    /// loop in server.rs / client.rs to receive an error on the next send and
    /// close the TCP connection cleanly.  Also removes the peer from the
    /// connections map.  Use for zombie peers that should not reconnect.
    ///
    /// Whitelisted (operator-trusted) peers are NEVER kicked — every kick path
    /// in the codebase ultimately routes through this function, so this is the
    /// single chokepoint that enforces the "whitelisted peers stay connected"
    /// invariant. Callers that need to drop a misbehaving peer regardless must
    /// remove it from the whitelist first.
    pub async fn kick_peer(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip);
        if self.is_whitelisted(ip_only).await {
            tracing::warn!(
                "⚠️ Suppressing kick of whitelisted peer {} (operator-trusted)",
                ip_only
            );
            return;
        }
        {
            let mut writers = self.peer_writers.write().await;
            writers.remove(ip_only);
        }
        if let Some(cm) = self.connection_manager() {
            cm.mark_disconnected(ip_only);
        }
        tracing::info!("🦵 Kicked zombie peer {} (writer channel closed)", ip_only);
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

        if let Some(writer_tx) = writers.get(ip_only) {
            // Pre-serialize the message into a length-prefixed frame
            let frame_bytes = crate::network::wire::serialize_frame(&message)?;

            writer_tx
                .send(frame_bytes)
                .map_err(|_| format!("Writer channel closed for peer {}", ip_only))?;

            Ok(())
        } else {
            tracing::debug!(
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
            // Rate-limit this warning to avoid log spam during bootstrap
            static LAST_NO_PEERS_BCAST: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = chrono::Utc::now().timestamp();
            let last = LAST_NO_PEERS_BCAST.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last >= 60 {
                LAST_NO_PEERS_BCAST.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                warn!("📡 Broadcast: no peers connected!");
            }
            return;
        }

        // Pre-serialize the message once for efficiency
        let msg_bytes = match crate::network::wire::serialize_frame(&message) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!("❌ Failed to serialize broadcast message: {}", e);
                return;
            }
        };

        let mut send_count = 0;
        let mut fail_count = 0;

        // Log for transaction broadcasts
        let is_tx_broadcast = matches!(message, NetworkMessage::TransactionBroadcast(_));

        for (peer_ip, writer_tx) in writers.iter() {
            if let Err(_e) = writer_tx.send(msg_bytes.clone()) {
                if is_tx_broadcast {
                    warn!("❌ TX broadcast to {} failed: channel closed", peer_ip);
                } else {
                    debug!("❌ Broadcast to {} failed: channel closed", peer_ip);
                }
                fail_count += 1;
                continue;
            }

            send_count += 1;
        }

        if is_tx_broadcast {
            info!(
                "📡 TX broadcast complete: {} peers sent, {} failed",
                send_count, fail_count
            );
        } else if send_count > 0 || fail_count > 0 {
            debug!(
                "📡 Broadcast complete: {} sent, {} failed",
                send_count, fail_count
            );
        }
    }

    /// Get list of connected peer IPs
    /// Only returns peers that have completed the handshake and have an active
    /// writer channel — i.e. truly connected, not just in the Connecting state.
    pub async fn get_connected_peers(&self) -> Vec<String> {
        let writers = self.peer_writers.read().await;
        writers
            .iter()
            .filter(|(_, w)| !w.is_closed())
            .map(|(ip, _)| ip.clone())
            .collect()
    }

    /// Get count of connected peers (post-handshake only)
    pub async fn peer_count(&self) -> usize {
        let writers = self.peer_writers.read().await;
        writers.values().filter(|w| !w.is_closed()).count()
    }

    /// Get a snapshot of connected peer IPs (for stats/monitoring)
    #[allow(dead_code)]
    pub async fn get_connected_peers_list(&self) -> Vec<String> {
        self.get_connected_peers().await
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
            tracing::debug!("❌ Peer {} not found in registry", ip_only);
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
