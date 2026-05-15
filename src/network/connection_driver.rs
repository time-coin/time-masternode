//! ConnectionDriver — bundles shared resources for outbound connection lifecycle management.
//!
//! `drive_outbound` consolidates the connection-establishment, message-loop, and cleanup
//! logic that previously lived in `client.rs::maintain_peer_connection`.  The spawn
//! wrapper in `client.rs` is now a thin shell responsible only for AI reconnection
//! metrics and AV3 coordinated-disconnect recording.

#![allow(dead_code)]

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::network::banlist::IPBanlist;
use crate::network::connection_manager::ConnectionManager;
use crate::network::peer_connection::{MessageLoopConfig, PeerConnection};
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::network::tls::TlsConfig;

/// Shared resources for managing the lifecycle of a single outbound connection.
pub struct ConnectionDriver {
    pub connection_manager: Arc<ConnectionManager>,
    pub masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    pub blockchain: Arc<crate::blockchain::Blockchain>,
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub banlist: Option<Arc<RwLock<IPBanlist>>>,
    pub tls_config: Option<Arc<TlsConfig>>,
    pub network_type: crate::network_type::NetworkType,
    pub ai_system: Option<Arc<crate::ai::AISystem>>,
}

impl ConnectionDriver {
    /// Establish an outbound connection to `ip:port`, run its message loop, then clean up.
    ///
    /// Returns `Ok(elapsed)` — the wall-clock duration the connection was live — so that
    /// the caller can feed the value to reconnection-AI success/failure recording.
    /// Returns `Err(reason)` when the connection could not be established at all.
    pub async fn drive_outbound(
        &self,
        ip: &str,
        port: u16,
        is_masternode: bool,
    ) -> Result<std::time::Duration, String> {
        let start = std::time::Instant::now();

        // Mark in peer_registry BEFORE attempting the connection to prevent a race with
        // a concurrent inbound from the same peer.
        if !self.peer_registry.mark_connecting(ip) {
            return Err(format!(
                "Already connecting/connected to {} in peer_registry",
                ip
            ));
        }

        // Create outbound connection.  Try TLS first; fall back to plaintext when the
        // remote rejects the handshake (e.g. an older build running plain TCP).
        // The server side already auto-detects TLS vs plaintext on inbound.
        let peer_conn = match PeerConnection::new_outbound(
            ip.to_string(),
            port,
            is_masternode,
            self.tls_config.clone(),
            self.network_type,
        )
        .await
        {
            Ok(conn) => conn,
            Err(e) if e.contains("TLS handshake failed") => {
                tracing::debug!(
                    "🔄 [OUTBOUND] TLS rejected by {}, retrying in plaintext",
                    ip
                );
                match PeerConnection::new_outbound(
                    ip.to_string(),
                    port,
                    is_masternode,
                    None,
                    self.network_type,
                )
                .await
                {
                    Ok(conn) => conn,
                    Err(e2) => {
                        self.peer_registry.unregister_peer(ip).await;
                        return Err(e2);
                    }
                }
            }
            Err(e) => {
                self.peer_registry.unregister_peer(ip).await;
                return Err(e);
            }
        };

        tracing::info!("✓ Connected to peer: {}", ip);

        let peer_ip = peer_conn.peer_ip().to_string();

        // Transition Connecting → Connected in the connection-state machine.
        self.connection_manager.mark_connected(&peer_ip);

        // Masternodes get relaxed ping timeouts and bypass some backoff checks.
        if is_masternode {
            self.connection_manager.mark_whitelisted(&peer_ip);
            tracing::debug!(
                "🛡️ Marked {} as whitelisted masternode with enhanced protection",
                peer_ip
            );
        }

        // Build the message-loop config using the builder pattern.
        let mut config = MessageLoopConfig::new(self.peer_registry.clone())
            .with_masternode_registry(self.masternode_registry.clone())
            .with_blockchain(self.blockchain.clone());

        if let Some(ref banlist) = self.banlist {
            config = config.with_banlist(banlist.clone());
        }

        if let (_, _, Some(broadcast_tx)) = self.peer_registry.get_timelock_resources().await {
            config = config.with_broadcast_rx(broadcast_tx.subscribe());
        }

        if let Some(ref ai) = self.ai_system {
            config = config.with_ai_system(ai.clone());
        }

        // Fresh per-connection rate limiter — mirrors the inbound check_rate_limit! path.
        let rate_limiter = Arc::new(RwLock::new(crate::network::rate_limiter::RateLimiter::new()));
        config = config.with_rate_limiter(rate_limiter);

        let result = peer_conn.run_message_loop_unified(config).await;

        // ── Cleanup ─────────────────────────────────────────────────────────────────

        self.connection_manager.mark_outbound_disconnected(&peer_ip);

        // Only clean up peer_registry if we're still the active outbound connection.
        // If the connection was superseded by an inbound (IP tiebreaker in
        // try_register_inbound), skip cleanup to avoid corrupting the new inbound.
        if self.peer_registry.is_outbound(&peer_ip) {
            self.peer_registry.mark_disconnected(&peer_ip);
            self.peer_registry.unregister_peer(&peer_ip).await;
        } else if !self.peer_registry.is_connected(&peer_ip) {
            self.peer_registry.unregister_peer(&peer_ip).await;
        } else {
            tracing::info!(
                "🔄 Outbound to {} superseded by inbound — skipping cleanup",
                peer_ip
            );
        }

        let elapsed = start.elapsed();

        // If this peer is a registered masternode, mark it inactive so it stops
        // receiving rewards until it reconnects.
        if self.masternode_registry.is_registered(&peer_ip).await {
            if let Err(e) = self
                .masternode_registry
                .mark_inactive_on_disconnect_with_duration(&peer_ip, Some(elapsed))
                .await
            {
                tracing::debug!("Could not mark masternode {} as inactive: {:?}", peer_ip, e);
            }
        }

        tracing::debug!("🔌 Unregistered peer {}", peer_ip);

        match result {
            Ok(_) => Ok(elapsed),
            Err(e) => Err(e),
        }
    }
}
