use super::context::MessageContext;
use super::MessageHandler;
use crate::network::message::NetworkMessage;
use tracing::{debug, warn};

impl MessageHandler {
    /// Handle Ping message - respond with Pong
    pub(super) async fn handle_ping(
        &self,
        nonce: u64,
        timestamp: i64,
        peer_height: Option<u64>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📨 [{}] Received ping from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        // Record per-peer clock drift from the timestamp the peer embedded in
        // the Ping.  A non-zero timestamp is required — zero means the peer
        // didn't include one.
        if timestamp != 0 {
            let drift = timestamp - chrono::Utc::now().timestamp();
            if let Some(tracker) = &context.drift_tracker {
                let mut t = tracker.lock().await;
                t.record(&self.peer_ip, drift);
                if t.is_drifted(&self.peer_ip) {
                    warn!(
                        "⚠️ [{}] Peer {} has persistent clock drift (avg >{:.0}s)",
                        self.direction,
                        self.peer_ip,
                        crate::time_sync::DRIFT_PENALTY_THRESHOLD_SECS
                    );
                    if let Some(ai) = &context.ai_system {
                        ai.attack_detector.record_timestamp(&self.peer_ip, drift);
                    }
                }
            }
        }

        // Update peer height if provided
        if let Some(h) = peer_height {
            context
                .peer_registry
                .update_peer_height(&self.peer_ip, h)
                .await;
        }

        // Include our height in pong response
        let our_height = context.blockchain.get_height();
        let pong = NetworkMessage::Pong {
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            height: Some(our_height),
        };

        debug!(
            "✅ [{}] Sent pong to {} (nonce: {}, height: {})",
            self.direction, self.peer_ip, nonce, our_height
        );

        Ok(Some(pong))
    }

    /// Handle Pong message - update peer height and RTT
    pub(super) async fn handle_pong(
        &self,
        nonce: u64,
        _timestamp: i64,
        peer_height: Option<u64>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📨 [{}] Received pong from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        // Update peer height if provided
        if let Some(h) = peer_height {
            context
                .peer_registry
                .update_peer_height(&self.peer_ip, h)
                .await;
        }

        // Record pong for centralized RTT tracking
        context
            .peer_registry
            .record_pong_received(&self.peer_ip, nonce)
            .await;

        Ok(None)
    }
}
