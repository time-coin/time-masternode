//! ConnectionDriver — bundles shared resources for outbound connection lifecycle management.
//!
//! `drive_outbound` consolidates the connection-establishment, message-loop, and cleanup
//! logic that previously lived in `client.rs::maintain_peer_connection`.  The spawn
//! wrapper in `client.rs` is now a thin shell responsible only for AI reconnection
//! metrics and AV3 coordinated-disconnect recording.
//!
//! `drive_inbound` mirrors the same pattern for inbound peers, consolidating the
//! 2500-line `handle_peer` body that previously lived in `server.rs`.

#![allow(dead_code)]

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

use crate::network::banlist::IPBanlist;
use crate::network::connection_manager::ConnectionManager;
use crate::network::message::NetworkMessage;
use crate::network::message_handler::{ConnectionDirection, MessageContext, MessageHandler};
use crate::network::peer_connection::{MessageLoopConfig, PeerConnection};
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::network::rate_limiter::RateLimiter;
use crate::network::tls::TlsConfig;
use crate::UTXOState;

/// Per-connection resources for an inbound peer that are NOT already on `ConnectionDriver`.
pub struct InboundResources {
    pub consensus: Arc<crate::consensus::ConsensusEngine>,
    pub peer_manager: Arc<crate::peer_manager::PeerManager>,
    pub banlist: Arc<tokio::sync::RwLock<crate::network::banlist::IPBanlist>>,
    pub broadcast_tx: tokio::sync::broadcast::Sender<crate::network::message::NetworkMessage>,
    pub seen_blocks: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    pub seen_transactions: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    pub seen_tx_finalized: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    pub seen_utxo_locks: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    pub local_ip: Option<String>,
    pub block_cache: Arc<crate::network::block_cache::BlockCache>,
    pub utxo_mgr: Arc<crate::utxo_manager::UTXOStateManager>,
    pub subs: Arc<
        tokio::sync::RwLock<
            std::collections::HashMap<String, crate::network::message::Subscription>,
        >,
    >,
}

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
    /// Returns `Ok(elapsed)` ΓÇö the wall-clock duration the connection was live ΓÇö so that
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
                    "≡ƒöä [OUTBOUND] TLS rejected by {}, retrying in plaintext",
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

        tracing::info!("Γ£ô Connected to peer: {}", ip);

        let peer_ip = peer_conn.peer_ip().to_string();

        // Transition Connecting ΓåÆ Connected in the connection-state machine.
        self.connection_manager.mark_connected(&peer_ip);

        // Masternodes get relaxed ping timeouts and bypass some backoff checks.
        if is_masternode {
            self.connection_manager.mark_whitelisted(&peer_ip);
            tracing::debug!(
                "≡ƒ¢í∩╕Å Marked {} as whitelisted masternode with enhanced protection",
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

        // Fresh per-connection rate limiter ΓÇö mirrors the inbound check_rate_limit! path.
        let rate_limiter = Arc::new(RwLock::new(crate::network::rate_limiter::RateLimiter::new()));
        config = config.with_rate_limiter(rate_limiter);

        let result = peer_conn.run_message_loop_unified(config).await;

        // ΓöÇΓöÇ Cleanup ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ

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
                "≡ƒöä Outbound to {} superseded by inbound ΓÇö skipping cleanup",
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

        tracing::debug!("≡ƒöî Unregistered peer {}", peer_ip);

        match result {
            Ok(_) => Ok(elapsed),
            Err(e) => Err(e),
        }
    }
    pub async fn drive_inbound(
        &self,
        stream: TcpStream,
        peer_addr: String,
        is_whitelisted: bool,
        mut notifier: broadcast::Receiver<NetworkMessage>,
        resources: InboundResources,
    ) -> Result<(), std::io::Error> {
        // Extract IP from address
        let ip: IpAddr = peer_addr
            .split(':')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| "127.0.0.1".parse().unwrap());

        let ip_str = ip.to_string();

        // Per-connection rate limiter: each peer gets its own instance, eliminating
        // the write-lock contention that the shared NetworkServer RateLimiter caused
        // under load (50+ peers ├ù multiple msg/s = constant write-lock contention).
        // The shared `rate_limiter` parameter is intentionally shadowed here.
        let rate_limiter = Arc::new(RwLock::new(RateLimiter::new()));

        // Get WebSocket tx event sender for real-time wallet notifications
        let ws_tx_event_sender = self.peer_registry.get_tx_event_sender().await;

        let _connection_start = std::time::Instant::now();

        // Wrap with TLS if configured
        // For TLS: split into separate reader and writer tasks using `tokio::io::split()`.
        // The original single-task `tokio::select!(read_message, write_rx.recv())` bridge
        // was NOT cancellation-safe: `read_message` calls `read_exact` internally, which
        // is documented as not cancellation-safe.  When a write became ready mid-frame the
        // select! branch would cancel `read_message` after consuming some bytes from the
        // stream, leaving the stream at an inconsistent offset.  The next `read_message`
        // call then read mid-payload bytes as a frame-length prefix ΓÇö producing the
        // deterministic 100 MBΓÇô3 GB "FrameBomb" sizes seen in production logs.
        // `tokio::io::split()` is safe here because rustls uses TLS 1.3 exclusively
        // (no renegotiation), so neither half ever needs cross-direction TLS I/O after
        // the handshake completes.
        // For non-TLS: `TcpStream::into_split()` is already correct (true full-duplex).
        let (msg_read_tx, mut msg_read_rx) =
            tokio::sync::mpsc::channel::<Result<Option<NetworkMessage>, String>>(512);
        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let writer_tx: crate::network::peer_connection_registry::PeerWriterTx = write_tx;

        if let Some(tls) = self.tls_config.clone() {
            match tls.accept_server(stream).await {
                Ok(tls_stream) => {
                    // Enforce SNI = "timecoin.local": every legitimate TIME node sets this
                    // exact SNI when opening an outbound TLS connection (peer_connection.rs).
                    // Any connection with a missing, blank, or different SNI (e.g. an IP
                    // address literal) is provably not one of our nodes ΓÇö it's a scanner,
                    // prober, or attacker.  Record an immediate violation so the IP
                    // escalates through temp ΓåÆ permanent ban (sled-persisted).
                    {
                        let sni = tls_stream.get_ref().1.server_name();
                        if sni != Some("timecoin.local") {
                            let sni_desc = sni.unwrap_or("<none>").to_owned();
                            if is_whitelisted {
                                // Whitelisted peers are operator-trusted; an unexpected
                                // SNI from one of them is almost certainly an older client
                                // build, not a probe. Log and continue.
                                tracing::warn!(
                                "ΓÜá∩╕Å  Whitelisted peer {} sent unexpected SNI {:?} ΓÇö accepting connection",
                                ip, sni_desc
                            );
                            } else {
                                resources.banlist.write().await.record_violation(
                                    ip,
                                    &format!("Invalid TLS SNI: {}", sni_desc),
                                );
                                if let Some(ref ai) = self.ai_system {
                                    ai.attack_detector.record_tls_failure(&ip_str);
                                }
                                tracing::debug!(
                                    "≡ƒÜ½ Rejected {} ΓÇö invalid SNI {:?} (not a TIME node)",
                                    ip,
                                    sni_desc
                                );
                                return Ok(());
                            }
                        }
                    }
                    // Log only after TLS succeeds ΓÇö plain TCP probes (reachability checks)
                    // would otherwise spam the log with connections that immediately fail TLS.
                    tracing::info!("≡ƒöî New peer connection from: {}", peer_addr);
                    tracing::debug!("≡ƒöÆ TLS established for inbound {}", peer_addr);
                    let gate_is_whitelisted = is_whitelisted;
                    let (mut tls_read, mut tls_write) = tokio::io::split(tls_stream);
                    // Spawn dedicated reader task ΓÇö reads without competing with writes,
                    // eliminating the cancellation-safety hazard of the old select! bridge.
                    let peer_addr_r = peer_addr.clone();
                    tokio::spawn(async move {
                        // Token-bucket flood gate: event-driven, no 1-second timer polling.
                        // Refills at GATE_RATE tokens/s; burst up to GATE_BURST.
                        // Soft-drops messages while tokens are exhausted; after
                        // GATE_HARD_DROPS consecutive soft-drops the peer is disconnected.
                        // Whitelisted peers get elevated limits AND never hard-kick ΓÇö a
                        // friendly node that briefly exceeds rate (e.g. mempool replay after
                        // reconnect, fork-resolution bursts) must stay connected so block
                        // production doesn't lose resources.consensus quorum.
                        let gate_rate: f64 = if gate_is_whitelisted { 5000.0 } else { 500.0 };
                        let gate_burst: f64 = if gate_is_whitelisted { 10000.0 } else { 1000.0 };
                        const GATE_HARD_DROPS: u32 = 500; // consecutive drops ΓåÆ hard kick
                        let mut gate_tokens: f64 = gate_burst;
                        let mut gate_last = std::time::Instant::now();
                        let mut gate_drop_streak: u32 = 0;
                        loop {
                            let result = crate::network::wire::read_message(&mut tls_read).await;
                            let is_eof = matches!(&result, Ok(None));
                            let is_err = result.is_err();
                            // Token-bucket refill: time-since-last-message, not a timer.
                            let gate_now = std::time::Instant::now();
                            let elapsed = gate_now.duration_since(gate_last).as_secs_f64();
                            gate_last = gate_now;
                            gate_tokens = (gate_tokens + elapsed * gate_rate).min(gate_burst);
                            if gate_tokens >= 1.0 {
                                gate_tokens -= 1.0;
                                gate_drop_streak = 0;
                            } else {
                                gate_drop_streak += 1;
                                if !gate_is_whitelisted && gate_drop_streak > GATE_HARD_DROPS {
                                    let _ = msg_read_tx
                                        .send(Err(
                                            "Message flood detected: pre-channel gate triggered"
                                                .to_string(),
                                        ))
                                        .await;
                                    break;
                                }
                                continue; // soft drop
                            }
                            if msg_read_tx.send(result).await.is_err() {
                                break; // receiver dropped
                            }
                            if is_eof || is_err {
                                break;
                            }
                        }
                        tracing::debug!("≡ƒöÆ TLS reader task exiting for {}", peer_addr_r);
                    });
                    // Spawn dedicated writer task.
                    let peer_addr_w = peer_addr.clone();
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        while let Some(data) = write_rx.recv().await {
                            if let Err(e) = tls_write.write_all(&data).await {
                                tracing::debug!("≡ƒöÆ TLS write error for {}: {}", peer_addr_w, e);
                                break;
                            }
                            if let Err(e) = tls_write.flush().await {
                                tracing::debug!("≡ƒöÆ TLS flush error for {}: {}", peer_addr_w, e);
                                break;
                            }
                        }
                        tracing::debug!("≡ƒöÆ TLS writer task exiting for {}", peer_addr_w);
                    });
                }
                Err(e) => {
                    // "handshake eof" is sent by old plain-TCP peers that don't speak TLS ΓÇö
                    // demote to DEBUG since it's expected noise from pre-TLS nodes and port scanners.
                    let e_str = e.to_string();
                    if e_str.contains("eof") || e_str.contains("early eof") {
                        tracing::debug!(
                            "≡ƒöô TLS handshake eof from {} (plain-TCP client?)",
                            peer_addr
                        );
                    } else {
                        tracing::warn!("≡ƒÜ½ TLS handshake failed for {}: {}", peer_addr, e);
                    }
                    // Charge a violation so repeat offenders accumulate bans.
                    // Without this, an attacker can flood TLS connections at zero cost ΓÇö
                    // each attempt consumes a tokio task + TLS negotiation with no penalty.
                    // Never record violations against our own IP ΓÇö self-connections (the node
                    // briefly attempting to connect to itself via the peer list) must not
                    // cause the node to permanently ban itself.
                    let is_self = resources.local_ip.as_deref().is_some_and(|l| l == ip_str);
                    if !is_self {
                        // Use the TLS-specific counter: much higher threshold, never permanent.
                        // TLS mode mismatches are operator config errors, not attacks ΓÇö using
                        // the standard record_violation path would permanently ban legitimate
                        // nodes after only 10 retries.
                        resources
                            .banlist
                            .write()
                            .await
                            .record_tls_violation(ip, &format!("TLS handshake failed: {}", e));
                        // Also feed into the AI detector ΓÇö it tracks distributed TLS floods
                        // from multiple IPs in the same /24 that each stay below the per-IP
                        // resources.banlist threshold (AV13 subnet variant).
                        if let Some(ref ai) = self.ai_system {
                            ai.attack_detector.record_tls_failure(&ip_str);
                        }
                    } else {
                        tracing::debug!(
                            "≡ƒöä Ignoring TLS failure from own IP {} (self-connection)",
                            ip_str
                        );
                    }
                    return Ok(());
                }
            }
        } else {
            tracing::debug!("≡ƒöô Plaintext connection from {}", peer_addr);
            let (r, w) = stream.into_split();
            // Spawn reader task for non-TLS
            let peer_addr_saved = peer_addr.clone();
            let peer_addr = peer_addr.clone();
            let gate_is_whitelisted = is_whitelisted;
            tokio::spawn(async move {
                let mut reader = r;
                // Token-bucket flood gate: event-driven, no 1-second timer polling.
                // Refills at gate_rate tokens/s; burst up to gate_burst.
                // Soft-drops messages while tokens are exhausted; after
                // GATE_HARD_DROPS consecutive soft-drops the peer is disconnected.
                // Whitelisted peers get elevated limits AND never hard-kick ΓÇö a
                // friendly node that briefly exceeds rate (e.g. mempool replay after
                // reconnect, fork-resolution bursts) must stay connected so block
                // production doesn't lose resources.consensus quorum.
                let gate_rate: f64 = if gate_is_whitelisted { 5000.0 } else { 500.0 };
                let gate_burst: f64 = if gate_is_whitelisted { 10000.0 } else { 1000.0 };
                const GATE_HARD_DROPS: u32 = 500; // consecutive drops ΓåÆ hard kick
                let mut gate_tokens: f64 = gate_burst;
                let mut gate_last = std::time::Instant::now();
                let mut gate_drop_streak: u32 = 0;
                loop {
                    let result = crate::network::wire::read_message(&mut reader).await;
                    let is_eof = matches!(&result, Ok(None));
                    let is_err = result.is_err();
                    // Token-bucket refill: time-since-last-message, not a timer.
                    let gate_now = std::time::Instant::now();
                    let elapsed = gate_now.duration_since(gate_last).as_secs_f64();
                    gate_last = gate_now;
                    gate_tokens = (gate_tokens + elapsed * gate_rate).min(gate_burst);
                    if gate_tokens >= 1.0 {
                        gate_tokens -= 1.0;
                        gate_drop_streak = 0;
                    } else {
                        gate_drop_streak += 1;
                        if !gate_is_whitelisted && gate_drop_streak > GATE_HARD_DROPS {
                            let _ = msg_read_tx
                                .send(Err("Message flood detected: pre-channel gate triggered"
                                    .to_string()))
                                .await;
                            break;
                        }
                        continue; // soft drop
                    }
                    if msg_read_tx.send(result).await.is_err() {
                        break;
                    }
                    if is_eof || is_err {
                        break;
                    }
                }
                tracing::debug!("≡ƒôû Reader task exiting for {}", peer_addr);
            });
            // Spawn writer task for non-TLS
            let peer_addr2 = peer_addr_saved.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let mut writer = w;
                while let Some(data) = write_rx.recv().await {
                    if let Err(e) = writer.write_all(&data).await {
                        tracing::debug!("≡ƒô¥ Write error for {}: {}", peer_addr2, e);
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        tracing::debug!("≡ƒô¥ Flush error for {}: {}", peer_addr2, e);
                        break;
                    }
                }
                tracing::debug!("≡ƒô¥ Writer task exiting for {}", peer_addr2);
            });
        }

        // ConnectionManager is the single authority for both inbound and outbound state.
        // Reject here ΓÇö after TLS succeeds but before any message work ΓÇö so that:
        //   (a) TLS failures / SNI rejections never consume a CM slot
        //   (b) duplicate connections are dropped before the writer channel is registered
        //   (c) can_accept_inbound (capacity only) in the accept loop stays a fast pre-check
        if !self
            .connection_manager
            .accept_inbound(&ip_str, is_whitelisted)
        {
            tracing::debug!(
            "≡ƒöä Dropping duplicate inbound from {} ΓÇö ConnectionManager already has a connection",
            peer_addr
        );
            return Ok(());
        }

        let mut handshake_done = false;
        let mut is_stable_connection = false;

        // Per-connection UTXO lock flood counter: tracks how many UTXOStateUpdate (Locked)
        // messages this peer has sent for each TX.  A legitimate TX with N inputs produces
        // exactly N lock messages ΓÇö an attacker who sends far more is DoS-flooding us.
        let mut peer_tx_lock_counts: std::collections::HashMap<[u8; 32], u32> =
            std::collections::HashMap::new();
        const MAX_UTXO_LOCKS_PER_TX: u32 = 50;

        let mut ping_excess_streak: u32 = 0;
        let mut tx_finalized_excess_streak: u32 = 0;

        // Per-connection rate-limit drop counters.  Declared here (outside the per-message
        // loop) so they accumulate across messages on the same connection, enabling the
        // suppression log ("N msgs suppressed in last 60s") and the record_severe_violation
        // escalation path (ΓëÑ10 drops) to actually fire.
        let mut rl_drop_count: u32 = 0;
        let mut rl_last_log = std::time::Instant::now() - std::time::Duration::from_secs(61);
        // Per-connection counter for invalid transactions. A single invalid tx is often a
        // legitimate race condition (stale UTXO, double-spend retry). Only record a resources.banlist
        // violation once a peer has sent 3 invalid transactions on the same connection.
        let mut invalid_tx_count: u32 = 0;

        let magic_bytes = self.network_type.magic_bytes();

        // A connection that completes TLS but never sends a Handshake message holds an open
        // tokio task and a connection slot indefinitely.  Fire a violation and close after 10s.
        let handshake_timeout = tokio::time::sleep(tokio::time::Duration::from_secs(10));
        tokio::pin!(handshake_timeout);

        loop {
            tokio::select! {
                result = msg_read_rx.recv() => {
                    let result = match result {
                        Some(r) => r,
                        None => {
                            if handshake_done {
                                tracing::info!("≡ƒöî Peer {} reader channel closed", peer_addr);
                            } else {
                                tracing::debug!("≡ƒöî Peer {} reader channel closed (pre-handshake)", peer_addr);
                            }
                            break;
                        }
                    };
                    match result {
                        Ok(None) => {
                            if handshake_done {
                                tracing::info!("≡ƒöî Peer {} disconnected (EOF)", peer_addr);
                            } else {
                                tracing::debug!("≡ƒöî Peer {} disconnected before handshake (EOF)", peer_addr);
                            }
                            break;
                        }
                        Err(e) => {
                            // Pre-handshake oversized frame: trivial 4-byte DoS ΓÇö penalise.
                            // Post-handshake frames > 100 MB are clearly malicious (e.g. 926 MB
                            // frames from fork-attack peers); penalise those too. Smaller
                            // post-handshake overflows may be a framing mismatch with older nodes
                            // and are not penalised so we don't ban legitimate sync peers.
                            const MALICIOUS_FRAME_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
                            let is_large_frame = e.contains("Frame too large");
                            let frame_bytes: Option<u64> = if is_large_frame {
                                e.split_whitespace()
                                    .find_map(|w| w.trim_end_matches("bytes").trim_end_matches(':').parse::<u64>().ok())
                            } else {
                                None
                            };
                            let clearly_malicious = frame_bytes.is_some_and(|b| b > MALICIOUS_FRAME_BYTES);
                            // Whitelisted oversized-frame events emit a single combined WARN below
                            // (via record_frame_bomb_violation); skip the generic "Connection ended"
                            // line to avoid 3-line-per-event log spam.
                            let suppress_generic_close_log = is_large_frame && is_whitelisted;
                            if !suppress_generic_close_log {
                                if handshake_done {
                                    tracing::info!("≡ƒöî Connection from {} ended: {}", peer_addr, e);
                                } else {
                                    tracing::debug!("≡ƒöî Connection from {} ended before handshake: {}", peer_addr, e);
                                }
                            }
                            if is_large_frame && (!handshake_done || clearly_malicious) {
                                if let Some(ai) = &self.ai_system {
                                    ai.attack_detector.record_frame_bomb(&ip_str);
                                }
                                if is_whitelisted {
                                    // Trusted (operator-owned) node: record_frame_bomb_violation()
                                    // applies a short temp ban via frame_bomb_bans (2 min / 15 min),
                                    // which bypasses the normal whitelist exemption in is_banned().
                                    // The stream is lost after a multi-GB length header; closing and
                                    // temp-banning forces a clean reconnect once the ban expires.
                                    resources.banlist.write().await.record_frame_bomb_violation(ip, &e);
                                } else {
                                    resources.banlist.write().await.record_violation(
                                        ip,
                                        &format!("Oversized frame header: {}", e),
                                    );
                                }
                            } else if e.contains("Message flood detected") {
                                // Distinguish legitimate burst traffic from raw flooding attacks:
                                // - Pre-handshake flood: the peer never authenticated ΓÇö raw TCP/protocol
                                //   flood (attacker). Record a violation so repeat offenders get banned.
                                // - Post-handshake burst: peer authenticated successfully and then sent
                                //   a large burst (e.g. syncing many blocks, retail checkout processing
                                //   many simultaneous transactions, mempool replay after reconnect).
                                //   This is normal network operation ΓÇö just disconnect and let them
                                //   reconnect.  Recording a banning violation would permanently cut
                                //   legitimate masternodes off from resources.consensus and rewards.
                                if is_whitelisted {
                                    tracing::debug!("≡ƒöî Message burst from whitelisted peer {} tripped pre-channel gate ΓÇö skipping penalty", peer_addr);
                                } else if !handshake_done {
                                    // Raw flood before any handshake ΓÇö likely an attacker.
                                    tracing::warn!("≡ƒîè Pre-handshake flood from {} ΓÇö recording violation", peer_addr);
                                    resources.banlist.write().await.record_violation(ip, "Message flood: pre-handshake flood");
                                    if let Some(ai) = &self.ai_system {
                                        ai.attack_detector.record_message_flood(&ip_str);
                                    }
                                } else {
                                    // Burst from authenticated peer ΓÇö normal operation (sync, commerce, etc.).
                                    // Do NOT feed record_message_flood() here: the AI enforcement loop
                                    // would call record_violation() and issue a ban despite the intent.
                                    tracing::info!("≡ƒîè Message burst from {} tripped pre-channel gate ΓÇö disconnecting (no ban, peer was authenticated)", peer_addr);
                                }
                            }
                            break;
                        }
                        Ok(Some(msg)) => {
                                // First message MUST be a valid handshake
                                if !handshake_done {
                                    match &msg {
                                        NetworkMessage::Handshake { magic, protocol_version, network, commit_count } => {
                                            if magic != &magic_bytes {
                                                if is_whitelisted {
                                                    tracing::warn!(
                                                        "ΓÜá∩╕Å  Whitelisted peer {} sent unexpected magic {:?} ΓÇö accepting anyway (operator-trusted)",
                                                        peer_addr, magic
                                                    );
                                                } else {
                                                    tracing::warn!("≡ƒÜ½ Rejecting {} - invalid magic bytes: {:?}", peer_addr, magic);
                                                    resources.banlist.write().await.record_violation(
                                                        ip,
                                                        &format!("Invalid magic bytes: {:?}", magic)
                                                    );
                                                    let _ = self.masternode_registry
                                                        .suspend_from_consensus(&ip_str, "Invalid handshake magic")
                                                        .await;
                                                    break;
                                                }
                                            }
                                            if *protocol_version < 2 {
                                                if is_whitelisted {
                                                    tracing::warn!(
                                                        "ΓÜá∩╕Å  Whitelisted peer {} on protocol version {} (minimum 2) ΓÇö accepting anyway",
                                                        peer_addr, protocol_version
                                                    );
                                                } else {
                                                    tracing::warn!(
                                                        "≡ƒÜ½ Rejecting {} ΓÇö protocol version {} is too old (minimum: 2). \
                                                        Please upgrade: https://github.com/time-coin/time-masternode",
                                                        peer_addr, protocol_version
                                                    );
                                                    // Send a human-readable upgrade notice before disconnecting.
                                                    let upgrade_msg = crate::network::message::NetworkMessage::ForkAlert {
                                                        your_height: 0,
                                                        your_hash: [0u8; 32],
                                                        consensus_height: 0,
                                                        consensus_hash: [0u8; 32],
                                                        consensus_peer_count: 0,
                                                        message: format!(
                                                            "Your node is using protocol version {protocol_version}, \
                                                            which is below the minimum required version (2). \
                                                            Please upgrade: https://github.com/time-coin/time-masternode"
                                                        ),
                                                    };
                                                    if let Ok(frame) = crate::network::wire::serialize_frame(&upgrade_msg) {
                                                        let _ = writer_tx.send(frame);
                                                    }
                                                    // Old software is not an attack ΓÇö do NOT record a violation.
                                                    // Recording violations here would permanently ban legitimate users
                                                    // who just haven't updated yet.
                                                    let _ = self.masternode_registry
                                                        .suspend_from_consensus(
                                                            &ip_str,
                                                            &format!("Protocol version {} below minimum 2", protocol_version),
                                                        )
                                                        .await;
                                                    break;
                                                }
                                            }
                                            let our_commits = env!("GIT_COMMIT_COUNT").parse::<u32>().unwrap_or(0);
                                            // Hard-reject peers below the minimum quorum version.
                                            // Nodes running old code cannot participate in resources.consensus
                                            // and must not be counted toward our peer quorum.
                                            // Whitelisted peers are operator-trusted infrastructure: never
                                            // close them on a version check ΓÇö even if they're behind, we
                                            // still want them in the peer set (they may still serve blocks
                                            // and votes in a degraded mode while the operator upgrades).
                                            if *commit_count < crate::constants::MIN_PEER_COMMIT_VERSION && !is_whitelisted {
                                                tracing::warn!(
                                                    "≡ƒÜ½ Rejecting {} ΓÇö running obsolete software \
                                                    (commit {}, minimum required: {}). \
                                                    Please upgrade: https://github.com/time-coin/time-masternode",
                                                    peer_addr, commit_count, crate::constants::MIN_PEER_COMMIT_VERSION
                                                );
                                                // Notify the peer so it can see the reason in its own logs.
                                                let upgrade_msg = crate::network::message::NetworkMessage::ForkAlert {
                                                    your_height: 0,
                                                    your_hash: [0u8; 32],
                                                    consensus_height: 0,
                                                    consensus_hash: [0u8; 32],
                                                    consensus_peer_count: 0,
                                                    message: format!(
                                                        "Your node (commit {commit_count}) is below the minimum \
                                                        required version ({min}). \
                                                        Please upgrade: https://github.com/time-coin/time-masternode",
                                                        min = crate::constants::MIN_PEER_COMMIT_VERSION
                                                    ),
                                                };
                                                if let Ok(frame) = crate::network::wire::serialize_frame(&upgrade_msg) {
                                                    let _ = writer_tx.send(frame);
                                                }
                                                // Old software is not an attack ΓÇö do NOT record a violation
                                                // (that escalates to a permanent ban). Instead, add a short
                                                // temp ban so the peer stops hammering us every ~30 s while
                                                // they haven't upgraded yet.  PHASE3 also checks the resources.banlist
                                                // before attempting outbound reconnects, so this one call
                                                // suppresses both the inbound spam and outbound reconnect
                                                // noise until the ban expires and they can try again.
                                                {
                                                    let reason = format!(
                                                        "Obsolete software: commit {} below minimum {}",
                                                        commit_count,
                                                        crate::constants::MIN_PEER_COMMIT_VERSION
                                                    );
                                                    resources.banlist.write().await.add_temp_ban(
                                                        ip,
                                                        Duration::from_secs(4 * 3600),
                                                        &reason,
                                                    );
                                                    let _ = self.masternode_registry
                                                        .suspend_from_consensus(&ip_str, &reason)
                                                        .await;
                                                }
                                                break;
                                            }
                                            if *commit_count < crate::constants::MIN_PEER_COMMIT_VERSION && is_whitelisted {
                                                tracing::warn!(
                                                    "ΓÜá∩╕Å  Whitelisted peer {} below minimum commit ({} < {}) ΓÇö accepting anyway (operator-trusted)",
                                                    peer_addr, commit_count, crate::constants::MIN_PEER_COMMIT_VERSION
                                                );
                                            }
                                            if *commit_count < our_commits {
                                                tracing::warn!(
                                                    "ΓÜá∩╕Å Peer {} is running outdated software \
                                                    (commit {}, we are at commit {}). \
                                                    Please upgrade: https://github.com/time-coin/time-masternode",
                                                    peer_addr, commit_count, our_commits
                                                );
                                            }
                                            // Check if the peer is ahead of us ΓÇö we may be outdated.
                                            if *commit_count > our_commits && our_commits > 0 {
                                                tracing::warn!(
                                                    "Γ¼å∩╕Å  Peer {} is running newer software \
                                                    (commit {}, we are at commit {}). \
                                                    Consider upgrading: https://github.com/time-coin/time-masternode",
                                                    peer_addr, commit_count, our_commits
                                                );
                                            }
                                            self.peer_registry.set_peer_commit_count(&ip_str, *commit_count).await;
                                            let _ = self.masternode_registry
                                                .clear_consensus_suspension(&ip_str)
                                                .await;
                                            tracing::info!(
                                                "Γ£à Handshake accepted from {} (network: {}, commit: {})",
                                                peer_addr, network, commit_count
                                            );
                                            handshake_done = true;

                                            // ConnectionManager is the single authority ΓÇö accept_inbound()
                                            // already ran at the top of handle_peer() and is the only
                                            // gate needed.  register_peer() below wires the writer channel
                                            // into PeerConnectionRegistry (the message router).
                                            tracing::info!("≡ƒô¥ Registering {} in PeerConnectionRegistry (peer_addr: {})", ip_str, peer_addr);
                                            self.peer_registry.register_peer(ip_str.clone(), writer_tx.clone()).await;
                                            tracing::debug!("Γ£à Successfully registered {} in registry", ip_str);

                                            // Send ACK to confirm handshake was processed
                                            let ack_msg = NetworkMessage::Ack {
                                                message_type: "Handshake".to_string(),
                                            };
                                            let _ = self.peer_registry.send_to_peer(&ip_str, ack_msg).await;

                                            // Load-balancing redirect: if we're above our soft
                                            // inbound limit and this is not a whitelisted peer,
                                            // send a PeerExchange of less-loaded alternatives and
                                            // close.  MIN_CONNECTIONS ensures the network never
                                            // fractures ΓÇö we always keep at least that many inbound.
                                            // Registered masternodes are never redirected ΓÇö they
                                            // are trusted peers that must stay connected for resources.consensus.
                                            const INBOUND_REDIRECT_THRESHOLD: usize = 175; // 70 % of MAX_INBOUND_CONNECTIONS
                                            const MIN_CONNECTIONS: usize = 8;
                                            let cur_inbound = self.peer_registry.inbound_count();
                                            let is_registered_masternode =
                                                self.masternode_registry.get(&ip_str).await.is_some();
                                            if !is_whitelisted
                                                && !is_registered_masternode
                                                && cur_inbound > INBOUND_REDIRECT_THRESHOLD
                                            {
                                                let alternatives =
                                                    self.peer_registry.get_peers_by_load(12).await;
                                                if alternatives.len() >= 3
                                                    && cur_inbound > MIN_CONNECTIONS
                                                {
                                                    tracing::info!(
                                                        "Γå⌐∩╕Å  Redirecting {} to {} less-loaded peers (inbound: {})",
                                                        ip_str, alternatives.len(), cur_inbound
                                                    );
                                                    let redirect = NetworkMessage::PeerExchange(alternatives);
                                                    let _ = self.peer_registry.send_to_peer(&ip_str, redirect).await;
                                                    // Small delay so the message can be flushed
                                                    tokio::time::sleep(
                                                        std::time::Duration::from_millis(200),
                                                    )
                                                    .await;
                                                    break; // Close after redirect
                                                }
                                            }

                                            // Send our masternode announcement if we're a masternode
                                            let local_address = self.masternode_registry.get_local_address().await;
                                            if let Some(our_address) = local_address {
                                                // Only send OUR masternode announcement, not all masternodes
                                                let local_masternodes = self.masternode_registry.get_all().await;
                                                if let Some(our_mn) = local_masternodes.iter().find(|mn| mn.masternode.address == our_address) {
                                                    let cert = self.masternode_registry.get_local_certificate().await;
                                                    // Use V4 if a collateral-claim proof has been submitted via RPC
                                                    let proof = our_mn.masternode.collateral_outpoint.as_ref()
                                                        .and_then(|op| self.masternode_registry.get_v4_proof(op))
                                                        .unwrap_or_default();
                                                    let announcement = if !proof.is_empty() {
                                                        NetworkMessage::MasternodeAnnouncementV4 {
                                                            address: our_mn.masternode.address.clone(),
                                                            reward_address: our_mn.reward_address.clone(),
                                                            tier: our_mn.masternode.tier,
                                                            public_key: our_mn.masternode.public_key,
                                                            collateral_outpoint: our_mn.masternode.collateral_outpoint.clone(),
                                                            certificate: cert.to_vec(),
                                                            started_at: self.masternode_registry.get_started_at(),
                                                            collateral_proof: proof.clone(),
                                                        }
                                                    } else {
                                                        NetworkMessage::MasternodeAnnouncementV3 {
                                                            address: our_mn.masternode.address.clone(),
                                                            reward_address: our_mn.reward_address.clone(),
                                                            tier: our_mn.masternode.tier,
                                                            public_key: our_mn.masternode.public_key,
                                                            collateral_outpoint: our_mn.masternode.collateral_outpoint.clone(),
                                                            certificate: cert.to_vec(),
                                                            started_at: self.masternode_registry.get_started_at(),
                                                        }
                                                    };
                                                    let version = if proof.is_empty() { "V3" } else { "V4 (with collateral proof)" };
                                                    let _ = self.peer_registry.send_to_peer(&ip_str, announcement).await;
                                                    tracing::info!("≡ƒôó Sent masternode announcement ({}) to peer {}", version, ip_str);
                                                }
                                            }

                                            // Request peer list for peer discovery
                                            let get_peers_msg = NetworkMessage::GetPeers;
                                            let _ = self.peer_registry.send_to_peer(&ip_str, get_peers_msg).await;

                                            // Request masternodes for peer discovery
                                            let get_mn_msg = NetworkMessage::GetMasternodes;
                                            let _ = self.peer_registry.send_to_peer(&ip_str, get_mn_msg).await;

                                            // Send our local mempool as a single bulk MempoolSyncResponse
                                            // so the peer can bootstrap its pool in one frame.  Using
                                            // individual TransactionFinalized messages here trips the
                                            // peer's tx_finalized rate limiter on every connect when the
                                            // mempool has ΓëÑ20 entries.
                                            {
                                                let entries = resources.consensus.get_all_for_sync();
                                                if !entries.is_empty() {
                                                    let msg = NetworkMessage::MempoolSyncResponse(entries);
                                                    let _ = self.peer_registry.send_to_peer(&ip_str, msg).await;
                                                }
                                            }

                                            // Ask the peer for their mempool so we learn about any
                                            // transactions they have that we don't.
                                            let mempool_req = NetworkMessage::MempoolSyncRequest;
                                            let _ = self.peer_registry.send_to_peer(&ip_str, mempool_req).await;

                                            // CRITICAL: Verify genesis hash compatibility EARLY
                                            // This prevents nodes with different genesis from exchanging blocks
                                            if self.blockchain.has_genesis() {
                                                let our_genesis_hash = self.blockchain.genesis_hash();
                                                // Request peer's genesis hash for verification
                                                let get_genesis_msg = NetworkMessage::GetGenesisHash;
                                                let _ = self.peer_registry.send_to_peer(&ip_str, get_genesis_msg).await;
                                                tracing::debug!(
                                                    "≡ƒôñ Requesting genesis hash from {} for compatibility check (our genesis: {})",
                                                    ip_str,
                                                    hex::encode(&our_genesis_hash[..8])
                                                );
                                            } else {
                                                // We don't have a genesis yet - request one from peer
                                                tracing::info!(
                                                    "≡ƒî▒ No local genesis - requesting genesis block from {}",
                                                    ip_str
                                                );
                                                let request_genesis_msg = NetworkMessage::RequestGenesis;
                                                let _ = self.peer_registry.send_to_peer(&ip_str, request_genesis_msg).await;
                                            }
                                            // Spawn periodic ping task for RTT measurement on this inbound connection.
                                            // Without this, ping times would only be tracked for outbound connections.
                                            {
                                                let ping_ip = ip_str.clone();
                                                let ping_registry = Arc::clone(&self.peer_registry);
                                                let ping_blockchain = Arc::clone(&self.blockchain);
                                                tokio::spawn(async move {
                                                    let mut interval = tokio::time::interval(
                                                        std::time::Duration::from_secs(30),
                                                    );
                                                    // Skip the immediate tick ΓÇö let the connection settle first.
                                                    interval.tick().await;
                                                    loop {
                                                        interval.tick().await;
                                                        if !ping_registry.is_connected(&ping_ip) {
                                                            break;
                                                        }
                                                        let nonce = rand::random::<u64>();
                                                        let height = ping_blockchain.get_height();
                                                        let msg = crate::network::message::NetworkMessage::Ping {
                                                            nonce,
                                                            timestamp: chrono::Utc::now().timestamp(),
                                                            height: Some(height),
                                                        };
                                                        ping_registry.record_ping_sent(&ping_ip, nonce).await;
                                                        if ping_registry.send_to_peer(&ping_ip, msg).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                });
                                            }
                                            continue;
                                        }
                                        _ => {
                                            tracing::warn!("ΓÜá∩╕Å  {} sent message before handshake - closing connection", peer_addr);
                                            if let Some(ref ai) = self.ai_system {
                                                ai.attack_detector.record_pre_handshake_violation(&ip_str);
                                            }
                                            // Record a direct resources.banlist violation per occurrence so
                                            // persistent pre-handshake probers accumulate bans even
                                            // if they disconnect before the 30s AI enforcement loop.
                                            {
                                                let mut bl = resources.banlist.write().await;
                                                bl.record_violation(ip, "Sent message before completing handshake");
                                            }
                                            break;
                                        }
                                    }
                                }

                                tracing::debug!("≡ƒôª Parsed message type from {}: {:?}", peer_addr, std::mem::discriminant(&msg));

                                // Phase 2.2: Rate limiting and resources.banlist enforcement
                                //
                                // rl_drop_count / rl_last_log are declared outside this loop so
                                // they accumulate across messages on the same connection.

                                // Define helper macro for rate limit checking with auto-ban
                                macro_rules! check_rate_limit {
                                    ($msg_type:expr) => {{
                                        let mut limiter = rate_limiter.write().await;
                                        let mut banlist_guard = resources.banlist.write().await;

                                        if !limiter.check($msg_type, &ip_str) {
                                            // Log at first occurrence in each 60s window.
                                            let now = std::time::Instant::now();
                                            let first_in_window = rl_drop_count == 0
                                                || now.duration_since(rl_last_log).as_secs() >= 60;
                                            if first_in_window {
                                                if rl_drop_count > 0 {
                                                    tracing::warn!(
                                                        "ΓÜá∩╕Å  Rate limit exceeded for {} from {} \
                                                         ({} msgs suppressed in last 60s)",
                                                        $msg_type, peer_addr, rl_drop_count
                                                    );
                                                } else {
                                                    tracing::warn!(
                                                        "ΓÜá∩╕Å  Rate limit exceeded for {} from {}",
                                                        $msg_type, peer_addr
                                                    );
                                                }
                                                rl_last_log = now;
                                            }
                                            rl_drop_count += 1;

                                            // Sync-safe messages are fundamental to network operation
                                            // (chain sync, peer discovery, liveness). Rate-limiting them
                                            // is correct ΓÇö dropping excess protects our resources ΓÇö but
                                            // they must never escalate to a ban. A syncing node legitimately
                                            // sends bursts of get_blocks and block messages during IBD; a
                                            // node coming online sends get_peers and pings. None of these
                                            // are abuse signals.
                                            //
                                            // Only abuse-prone messages (tx spam, announce spam, etc.)
                                            // should score violations when sustained rate-limiting occurs.
                                            let is_sync_safe = matches!(
                                                $msg_type,
                                                "get_blocks"
                                                    | "block"
                                                    | "get_peers"
                                                    | "ping"
                                                    | "pong"
                                                    | "genesis_request"
                                            );

                                            let should_ban = if is_sync_safe {
                                                false // Never ban for sync/discovery/liveness bursts
                                            } else if rl_drop_count >= 10 && !is_whitelisted {
                                                banlist_guard.record_severe_violation(ip,
                                                    &format!("Mass flood: {} ({}+ msgs dropped)", $msg_type, rl_drop_count))
                                            } else if rl_drop_count >= 5 && !is_whitelisted {
                                                // Sustained rate-limiting of an abuse-prone message type.
                                                banlist_guard.record_violation(ip,
                                                    &format!("Sustained rate-limit flood: {} ({} msgs dropped)", $msg_type, rl_drop_count))
                                            } else {
                                                false
                                            };

                                            if should_ban && !is_whitelisted {
                                                tracing::warn!(
                                                    "≡ƒÜ½ Disconnecting {} due to rate limit flood ({} dropped)",
                                                    peer_addr, rl_drop_count
                                                );
                                                drop(banlist_guard);
                                                drop(limiter);
                                                self.peer_registry.kick_peer(&ip_str).await;
                                                break; // Exit connection loop
                                            }
                                            continue; // Skip processing this message
                                        }

                                        drop(limiter);
                                        drop(banlist_guard);
                                    }};
                                }

                                // Size validation handled by wire protocol (4MB max frame)
                                macro_rules! check_message_size {
                                    ($max_size:expr, $msg_type:expr) => {{}};
                                }

                                match &msg {
                                    // PRIORITY: UTXO locks MUST be processed immediately, even during block sync
                                    // This prevents double-spend race conditions
                                    NetworkMessage::UTXOStateUpdate { outpoint, state } => {
                                        // Create unique identifier for this UTXO lock update
                                        let mut lock_id = Vec::new();
                                        lock_id.extend_from_slice(&outpoint.txid);
                                        lock_id.extend_from_slice(&outpoint.vout.to_le_bytes());

                                        // Add state discriminant to differentiate lock types
                                        match state {
                                            UTXOState::Locked { txid, .. } => {
                                                lock_id.push(1); // Locked state
                                                lock_id.extend_from_slice(txid);
                                            }
                                            UTXOState::Unspent => lock_id.push(2),
                                            UTXOState::SpentPending { txid, .. } => {
                                                lock_id.push(3);
                                                lock_id.extend_from_slice(txid);
                                            }
                                            UTXOState::SpentFinalized { txid, .. } => {
                                                lock_id.push(4);
                                                lock_id.extend_from_slice(txid);
                                            }
                                            UTXOState::Archived { txid, .. } => {
                                                lock_id.push(5);
                                                lock_id.extend_from_slice(txid);
                                            }
                                        }

                                        // Check if we've already processed this exact UTXO lock update
                                        let already_seen = resources.seen_utxo_locks.check_and_insert(&lock_id).await;

                                        if already_seen {
                                            tracing::debug!("≡ƒöü Ignoring duplicate UTXO lock update from {}", peer_addr);
                                            continue;
                                        }

                                        // FLOOD GUARD: count distinct Locked messages per TX from this peer.
                                        // A legitimate TX with N inputs sends exactly N lock messages.
                                        // Anything beyond MAX_UTXO_LOCKS_PER_TX is a DoS flood ΓÇö flag and drop.
                                        if let UTXOState::Locked { txid, .. } = &state {
                                            let count = peer_tx_lock_counts.entry(*txid).or_insert(0);
                                            *count += 1;
                                            if *count > MAX_UTXO_LOCKS_PER_TX {
                                                if let Some(ref ai) = self.ai_system {
                                                    ai.attack_detector.record_utxo_lock_flood(
                                                        &ip_str,
                                                        &hex::encode(txid),
                                                        *count,
                                                    );
                                                }
                                                tracing::warn!(
                                                    "≡ƒÜ½ UTXO lock flood from {}: {} locks for TX {} (max {}), dropping",
                                                    peer_addr,
                                                    count,
                                                    hex::encode(txid),
                                                    MAX_UTXO_LOCKS_PER_TX
                                                );
                                                continue;
                                            }
                                        }

                                        tracing::debug!("≡ƒöÆ PRIORITY: Received UTXO lock update from {}", peer_addr);
                                        resources.consensus.utxo_manager.update_state(outpoint, state.clone());

                                        if let UTXOState::Locked { txid, .. } = state {
                                            tracing::debug!(
                                                "≡ƒöÆ Applied UTXO lock from peer {} for TX {}",
                                                peer_addr,
                                                hex::encode(txid)
                                            );
                                        }

                                        // Gossip lock to other peers immediately (only if not duplicate)
                                        let _ = resources.broadcast_tx.send(msg.clone());
                                    }

                                    NetworkMessage::Ack { message_type } => {
                                        tracing::debug!("Γ£à Received ACK for {} from {}", message_type, peer_addr);
                                        // ACKs are informational, no action needed
                                    }
                                    NetworkMessage::TransactionBroadcast(tx) => {
                                        check_message_size!(MAX_TX_SIZE, "Transaction");
                                        check_rate_limit!("tx");

                                        // AV40: Structural check before dedup ΓÇö block null TXs
                                        // at the gate so we never gossip structurally invalid payloads.
                                        if tx.inputs.is_empty() || tx.outputs.is_empty() {
                                            tracing::debug!(
                                                "≡ƒùæ∩╕Å Null TX from {} rejected (0 inputs or 0 outputs)",
                                                peer_addr
                                            );
                                            if let Some(ref ai) = self.ai_system {
                                                ai.attack_detector.record_null_tx_flood(&ip_str);
                                            }
                                            continue;
                                        }

                                        // Check if we've already seen this transaction using Bloom filter
                                        let txid = tx.txid();
                                        let already_seen = resources.seen_transactions.check_and_insert(&txid).await;

                                        if already_seen {
                                            tracing::debug!("≡ƒöü Ignoring duplicate transaction {} from {}", hex::encode(txid), peer_addr);
                                            continue;
                                        }

                                        tracing::info!("≡ƒôÑ Received new transaction {} from {}", hex::encode(txid), peer_addr);

                                        // Avalanche-style gossip: relay immediately before local processing.
                                        // Transactions spread even if this node temporarily rejects them
                                        // (e.g., UTXO not yet indexed during sync catch-up). The
                                        // resources.seen_transactions bloom filter prevents relay loops.
                                        match resources.broadcast_tx.send(msg.clone()) {
                                            Ok(receivers) => {
                                                tracing::debug!("≡ƒöä Gossiped transaction {} to {} peer(s)", hex::encode(txid), receivers.saturating_sub(1));
                                            }
                                            Err(e) => {
                                                tracing::debug!("Failed to gossip transaction: {}", e);
                                            }
                                        }

                                        // Skip local mempool processing while syncing ΓÇö UTXOs are not
                                        // yet indexed so resources.consensus would reject the TX anyway. We have
                                        // already gossiped above, so the TX continues to spread.
                                        if self.blockchain.is_syncing() {
                                            tracing::debug!(
                                                "ΓÅ¡∩╕Å Skipping local TX processing from {} ΓÇö node is syncing",
                                                peer_addr
                                            );
                                            continue;
                                        }

                                        // Process transaction locally (validates, adds to mempool,
                                        // initiates TimeVote if we are a masternode).
                                        match resources.consensus.process_transaction(tx.clone(), None).await {
                                            Ok(_) => {
                                                tracing::debug!("Γ£à Transaction {} processed", hex::encode(txid));

                                                // Emit WebSocket notification for subscribed wallets
                                                if let Some(ref tx_sender) = ws_tx_event_sender {
                                                    let outputs: Vec<crate::rpc::websocket::TxOutputInfo> = tx
                                                        .outputs
                                                        .iter()
                                                        .enumerate()
                                                        .map(|(i, out)| {
                                                            let address = String::from_utf8(out.script_pubkey.clone())
                                                                .unwrap_or_else(|_| hex::encode(&out.script_pubkey));
                                                            crate::rpc::websocket::TxOutputInfo {
                                                                address,
                                                                amount: out.value as f64 / 100_000_000.0,
                                                                index: i as u32,
                                                            }
                                                        })
                                                        .collect();

                                                    let event = crate::rpc::websocket::TransactionEvent {
                                                        txid: hex::encode(txid),
                                                        outputs,
                                                        timestamp: chrono::Utc::now().timestamp(),
                                                        status: crate::rpc::websocket::TxEventStatus::Pending,
                                                    };
                                                    match tx_sender.send(event) {
                                                        Ok(n) => tracing::info!("≡ƒôí WS tx_notification (server.rs) sent to {} receiver(s)", n),
                                                        Err(_) => tracing::warn!("≡ƒôí WS tx_notification (server.rs) failed: no receivers"),
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                let err_str = e.to_string();
                                                if err_str.contains("already in pool") || err_str.contains("Already") {
                                                    tracing::debug!("≡ƒöü Transaction {} already in pool (from {})", hex::encode(txid), peer_addr);
                                                } else if err_str.contains("no inputs") || err_str.contains("no outputs") {
                                                    // Structural check above should have caught this ΓÇö
                                                    // log for diagnostics but don't penalise the relayer.
                                                    tracing::debug!(
                                                        "≡ƒùæ∩╕Å Null TX {} from {} rejected locally ({})",
                                                        hex::encode(txid), peer_addr, err_str
                                                    );
                                                    if let Some(ref ai) = self.ai_system {
                                                        ai.attack_detector.record_null_tx_flood(&ip_str);
                                                    }
                                                } else {
                                                    tracing::warn!("Γ¥î Transaction {} rejected locally: {}", hex::encode(txid), e);

                                                    // Only record a violation after 3 invalid transactions
                                                    // on this connection. A single invalid tx is often a
                                                    // legitimate race condition (stale UTXO, rebroadcast of
                                                    // an already-finalized tx). Persistent submission of
                                                    // invalid transactions indicates abuse.
                                                    invalid_tx_count += 1;
                                                    if invalid_tx_count >= 3 {
                                                        let mut banlist_guard = resources.banlist.write().await;
                                                        let should_ban = banlist_guard.record_violation(ip, "Repeated invalid transactions");
                                                        drop(banlist_guard);
                                                        if let Some(ref ai) = self.ai_system {
                                                            ai.attack_detector.record_invalid_tx_spam(&ip_str);
                                                        }
                                                        if should_ban {
                                                            tracing::warn!("≡ƒÜ½ Disconnecting {} due to repeated invalid transactions ({})", peer_addr, invalid_tx_count);
                                                            self.peer_registry.kick_peer(&ip_str).await;
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    NetworkMessage::TransactionFinalized { txid, tx } => {
                                        // AV40: inline rate check with AI escalation.
                                        // The generic check_rate_limit! macro calls record_violation()
                                        // which is subject to whitelist exemption and never reaches
                                        // record_finality_injection().  By inlining here (mirroring
                                        // ping_excess_streak) we feed sustained flooding into the
                                        // AV38 relay-safe two-tier escalation in AttackDetector.
                                        {
                                            let rate_ok = {
                                                let mut limiter = rate_limiter.write().await;
                                                limiter.check("tx_finalized", &ip_str)
                                            };
                                            if !rate_ok {
                                                tx_finalized_excess_streak += 1;
                                                tracing::warn!(
                                                    "ΓÜá∩╕Å  Rate limit exceeded for tx_finalized from {} (excess streak: {})",
                                                    peer_addr, tx_finalized_excess_streak
                                                );
                                                if tx_finalized_excess_streak >= 5 {
                                                    if let Some(ref ai) = self.ai_system {
                                                        ai.attack_detector.record_finality_injection(&ip_str);
                                                    }
                                                    // Direct escalation: each 5-excess cycle is a
                                                    // sustained flood ΓÇö record a resources.banlist violation
                                                    // so the ban escalates without waiting for the
                                                    // 30s AI enforcement loop.  Legitimate relays
                                                    // never hit 5 consecutive rate-limit excess.
                                                    let should_disconnect = resources.banlist
                                                        .write()
                                                        .await
                                                        .record_violation(
                                                            ip,
                                                            "tx_finalized flood: sustained rate-limit excess (AV38+AV40)",
                                                        );
                                                    tx_finalized_excess_streak = 0;
                                                    if should_disconnect {
                                                        tracing::warn!(
                                                            "≡ƒÜ½ Disconnecting {} ΓÇö repeated tx_finalized flooding (AV38+AV40)",
                                                            peer_addr
                                                        );
                                                        self.peer_registry.kick_peer(&ip_str).await;
                                                        break;
                                                    }
                                                }
                                                continue;
                                            }
                                            tx_finalized_excess_streak = 0;
                                        }

                                        // Drop mempool transactions while syncing ΓÇö the UTXOs they
                                        // reference likely don't exist in our local UTXO set yet, so
                                        // every validation would fail with "input not in storage".
                                        // The peer will re-broadcast once we're caught up.
                                        if self.blockchain.is_syncing() {
                                            tracing::debug!("ΓÅ¡∩╕Å Skipping TransactionFinalized {} ΓÇö node is syncing", hex::encode(*txid));
                                            continue;
                                        }

                                        // Dedup: skip if we've already processed this finalization
                                        let already_seen = resources.seen_tx_finalized.check_and_insert(txid).await;
                                        if already_seen {
                                            tracing::debug!("≡ƒöü Ignoring duplicate TransactionFinalized {} from {}", hex::encode(*txid), peer_addr);
                                            continue;
                                        }

                                        tracing::info!("Γ£à Transaction {} finalized (from {})",
                                            hex::encode(*txid), peer_addr);

                                        // AV38+AV40 combined guard: drop null TXs that arrive as
                                        // TransactionFinalized.  The attacker feeds honest relay nodes
                                        // with null TXs (0 inputs, 0 outputs, no special_data), which
                                        // those nodes then re-broadcast as TransactionFinalized.  By
                                        // dropping here we stop the amplification WITHOUT banning the
                                        // relay (which is also a victim).  The AI tracker records the
                                        // event at a relay-safe threshold so only the true source is
                                        // eventually penalised.
                                        if tx.inputs.is_empty() && tx.outputs.is_empty() && tx.special_data.is_none() {
                                            tracing::debug!(
                                                "≡ƒùæ∩╕Å Null TX {} via TransactionFinalized from {} ΓÇö dropped (AV38+AV40)",
                                                hex::encode(*txid), peer_addr
                                            );
                                            if let Some(ref ai) = self.ai_system {
                                                ai.attack_detector.record_finality_injection(&ip_str);
                                            }
                                            continue;
                                        }

                                        // AV41/AV48: ghost special_data guard. Rejects:
                                        //   Phase 1 ΓÇö empty/malformed fields (validate_fields)
                                        //   Phase 2 ΓÇö forged signature (verify_signature)
                                        //   Phase 3 ΓÇö fresh keypair with mismatched wallet_address (verify_address_binding)
                                        if tx.inputs.is_empty() && tx.outputs.is_empty() {
                                            let sig_ok =
                                                tx.special_data.as_ref().is_some_and(|sd| {
                                                    sd.validate_fields().is_ok()
                                                        && sd.verify_signature().is_ok()
                                                        && sd.verify_address_binding().is_ok()
                                                });
                                            if !sig_ok {
                                                tracing::debug!(
                                                    "≡ƒùæ∩╕Å Ghost/forged special_data TX {} via TransactionFinalized from {} ΓÇö dropped (AV41)",
                                                    hex::encode(*txid), peer_addr
                                                );
                                                if let Some(ref ai) = self.ai_system {
                                                    ai.attack_detector.record_finality_injection(&ip_str);
                                                }
                                                continue;
                                            }
                                        }

                                        // If the TX is already in the finalized pool, skip entirely
                                        if resources.consensus.tx_pool.is_finalized(txid) {
                                            tracing::debug!("≡ƒôª TX {} already in finalized pool, skipping", hex::encode(*txid));
                                            // Still gossip so other peers learn about it
                                            let _ = resources.broadcast_tx.send(msg.clone());
                                            continue;
                                        }

                                        // Check whether all input UTXOs are accounted for locally.
                                        // Tombstoned inputs are legitimately spent (removed from sled
                                        // storage by mark_timevote_finalized) and must NOT be treated
                                        // as a UTXO-set divergence ΓÇö doing so caused the TX to skip
                                        // confirmed-pool insertion, leaving it stuck forever.
                                        // Only flag divergence when an input is neither in storage
                                        // nor tombstoned (i.e. genuinely unknown to this node).
                                        let mut inputs_exist = true;
                                        for input in &tx.inputs {
                                            let in_storage = resources.consensus.utxo_manager.get_utxo(&input.previous_output).await.is_ok();
                                            let tombstoned = resources.consensus.utxo_manager.is_tombstoned(&input.previous_output);
                                            if !in_storage && !tombstoned {
                                                tracing::warn!(
                                                    "ΓÜá∩╕Å TransactionFinalized {} from {}: input {} not in local storage \
                                                     and not tombstoned (UTXO set diverged) ΓÇö will apply outputs without \
                                                     marking inputs spent",
                                                    hex::encode(*txid), peer_addr, input.previous_output
                                                );
                                                inputs_exist = false;
                                                break;
                                            }
                                        }
                                        if !inputs_exist {
                                            // Apply outputs directly so the recipient wallet sees the
                                            // new UTXOs even while our local set is diverged.
                                            for (idx, output) in tx.outputs.iter().enumerate() {
                                                let outpoint = crate::types::OutPoint {
                                                    txid: *txid,
                                                    vout: idx as u32,
                                                };
                                                let utxo = crate::types::UTXO {
                                                    outpoint: outpoint.clone(),
                                                    value: output.value,
                                                    script_pubkey: output.script_pubkey.clone(),
                                                    address: String::from_utf8(output.script_pubkey.clone())
                                                        .unwrap_or_default(),
                                                masternode_key: None,
                                                };
                                                if let Err(e) = resources.consensus.utxo_manager.add_utxo(utxo).await {
                                                    tracing::warn!(
                                                        "Failed to add output UTXO vout={} for diverged TX {}: {}",
                                                        idx, hex::encode(*txid), e
                                                    );
                                                } else {
                                                    resources.consensus.utxo_manager.update_state(&outpoint, crate::types::UTXOState::Unspent);
                                                }
                                            }
                                            // Gossip so other peers can also learn about this finalization
                                            let msg = NetworkMessage::TransactionFinalized {
                                                txid: *txid,
                                                tx: tx.clone(),
                                            };
                                            let _ = resources.broadcast_tx.send(msg);
                                            continue;
                                        }

                                        // Add to pool if not present.  We deliberately do NOT call
                                        // process_transaction() here ΓÇö that would broadcast a
                                        // TimeVoteRequest to all validators, giving a 49x amplification
                                        // to an attacker who injects TransactionFinalized for unknown
                                        // TXs (AV38: Finality Injection).  Instead, add directly to the
                                        // pending pool and let the manual finalization path below handle
                                        // the UTXO state transitions.
                                        let auto_finalized = if !resources.consensus.tx_pool.has_transaction(txid) {
                                            tracing::warn!(
                                                "ΓÜá∩╕Å TransactionFinalized for unknown TX {} from {} ΓÇö \
                                                 adding to pool without resources.consensus re-broadcast (AV38 guard)",
                                                hex::encode(*txid), peer_addr
                                            );
                                            // Record for AI sliding-window detection (AV38).
                                            if let Some(ref ai) = self.ai_system {
                                                ai.attack_detector.record_finality_injection(&ip_str);
                                            }
                                            // Add directly without triggering TimeVote broadcast.
                                            let _ = resources.consensus.tx_pool.add_pending(tx.clone(), 0);
                                            false // let the manual finalization path below run
                                        } else {
                                            false
                                        };

                                        // Only do manual finalization if process_transaction didn't already do it.
                                        // This prevents double-finalization (creating output UTXOs twice).
                                        if !auto_finalized {
                                            if resources.consensus.tx_pool.finalize_transaction(*txid) {
                                                tracing::info!("≡ƒôª Moved TX {} to finalized pool on this node", hex::encode(*txid));

                                                // Transition input UTXOs ΓåÆ SpentFinalized and
                                                // remove from sled storage + address_index so they
                                                // are not resurrected as Unspent after a node restart.
                                                for input in &tx.inputs {
                                                    resources.consensus.utxo_manager
                                                        .mark_timevote_finalized(&input.previous_output, *txid)
                                                        .await;
                                                }

                                                // Create output UTXOs
                                                for (idx, output) in tx.outputs.iter().enumerate() {
                                                    let outpoint = crate::types::OutPoint {
                                                        txid: *txid,
                                                        vout: idx as u32,
                                                    };
                                                    let utxo = crate::types::UTXO {
                                                        outpoint: outpoint.clone(),
                                                        value: output.value,
                                                        script_pubkey: output.script_pubkey.clone(),
                                                        address: String::from_utf8(output.script_pubkey.clone())
                                                            .unwrap_or_default(),
                                                    masternode_key: None,
                                                    };
                                                    if let Err(e) = resources.consensus.utxo_manager.add_utxo(utxo).await {
                                                        tracing::warn!("Failed to add output UTXO vout={}: {}", idx, e);
                                                    }
                                                    resources.consensus.utxo_manager.update_state(&outpoint, crate::types::UTXOState::Unspent);
                                                }
                                            } else {
                                                tracing::debug!("ΓÜá∩╕Å Could not finalize TX {} (not in pending pool)", hex::encode(*txid));
                                            }
                                        }

                                        // Notify WS subscribers on this node that the transaction is finalized
                                        resources.consensus.signal_tx_finalized(*txid);

                                        // Gossip finalization to other peers
                                        match resources.broadcast_tx.send(msg.clone()) {
                                            Ok(receivers) => {
                                                tracing::debug!("≡ƒöä Gossiped finalization to {} peer(s)", receivers.saturating_sub(1));
                                            }
                                            Err(e) => {
                                                tracing::debug!("Failed to gossip finalization: {}", e);
                                            }
                                        }
                                    }
                                    NetworkMessage::UTXOStateQuery(_) => {
                                        check_rate_limit!("utxo_query");

                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.utxo_manager = Some(Arc::clone(&resources.utxo_mgr));

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::Subscribe(sub) => {
                                        check_rate_limit!("subscribe");
                                        resources.subs.write().await.insert(sub.id.clone(), sub.clone());
                                    }
                                    NetworkMessage::GetBlockHeight => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetChainTip => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetPendingTransactions => {
                                        // Get pending transactions from mempool
                                        let pending_txs = self.blockchain.get_pending_transactions();
                                        let reply = NetworkMessage::PendingTransactionsResponse(pending_txs);
                                        let _ = self.peer_registry.send_to_peer(&ip_str, reply).await;
                                    }
                                    NetworkMessage::GetBlocks(_start, _end) => {
                                        check_rate_limit!("get_blocks");

                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetUTXOStateHash => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetUTXOSet => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::MasternodeAnnouncement { .. } => {
                                        // V1 deprecated ΓÇö all nodes use V2 now
                                        tracing::debug!("ΓÅ¡∩╕Å  Ignoring deprecated V1 masternode announcement from {}", peer_addr);
                                    }
                                    NetworkMessage::MasternodeAnnouncementV2 { address: _, reward_address, tier, public_key, collateral_outpoint } => {
                                        // V2 without certificate ΓÇö delegate to V3 handler with empty cert
                                        let v3_msg = NetworkMessage::MasternodeAnnouncementV3 {
                                            address: peer_addr.split(':').next().unwrap_or("").to_string(),
                                            reward_address: reward_address.clone(),
                                            tier: *tier,
                                            public_key: *public_key,
                                            collateral_outpoint: collateral_outpoint.clone(),
                                            certificate: vec![0u8; 64],
                                            started_at: 0,
                                        };
                                        // Fall through to V3 handler below
                                        // (re-dispatch via the message handler for consistency)
                                        check_rate_limit!("masternode_announce");
                                        if !is_stable_connection {
                                            is_stable_connection = true;
                                        }
                                        let peer_ip_str = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.utxo_manager = Some(Arc::clone(&resources.consensus.utxo_manager));
                                        context.peer_manager = Some(Arc::clone(&resources.peer_manager));
                                        match handler.handle_message(&v3_msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&peer_addr, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::MasternodeAnnouncementV3 { address: _, reward_address, tier, public_key, collateral_outpoint, certificate, started_at } => {
                                        check_rate_limit!("masternode_announce");
                                        if !is_stable_connection {
                                            is_stable_connection = true;
                                        }
                                        let peer_ip_str = peer_addr.split(':').next().unwrap_or("").to_string();
                                        if peer_ip_str.is_empty() { continue; }
                                        // Delegate to unified message handler (same path as V2)
                                        let v3_msg = NetworkMessage::MasternodeAnnouncementV3 {
                                            address: peer_ip_str.clone(),
                                            reward_address: reward_address.clone(),
                                            tier: *tier,
                                            public_key: *public_key,
                                            collateral_outpoint: collateral_outpoint.clone(),
                                            certificate: certificate.clone(),
                                            started_at: *started_at,
                                        };
                                        let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.utxo_manager = Some(Arc::clone(&resources.consensus.utxo_manager));
                                        context.peer_manager = Some(Arc::clone(&resources.peer_manager));
                                        match handler.handle_message(&v3_msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&peer_addr, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::MasternodeAnnouncementV4 { address: _, reward_address, tier, public_key, collateral_outpoint, certificate, started_at, collateral_proof } => {
                                        check_rate_limit!("masternode_announce");
                                        if !is_stable_connection {
                                            is_stable_connection = true;
                                        }
                                        let peer_ip_str = peer_addr.split(':').next().unwrap_or("").to_string();
                                        if peer_ip_str.is_empty() { continue; }
                                        let v4_msg = NetworkMessage::MasternodeAnnouncementV4 {
                                            address: peer_ip_str.clone(),
                                            reward_address: reward_address.clone(),
                                            tier: *tier,
                                            public_key: *public_key,
                                            collateral_outpoint: collateral_outpoint.clone(),
                                            certificate: certificate.clone(),
                                            started_at: *started_at,
                                            collateral_proof: collateral_proof.clone(),
                                        };
                                        let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.utxo_manager = Some(Arc::clone(&resources.consensus.utxo_manager));
                                        context.peer_manager = Some(Arc::clone(&resources.peer_manager));
                                        match handler.handle_message(&v4_msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&peer_addr, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::MempoolSyncRequest => {
                                        // Peer is asking for our current pool contents.
                                        // Respond with a single MempoolSyncResponse bulk frame instead
                                        // of individual TransactionFinalized/TransactionBroadcast
                                        // messages; the per-message approach tripped the peer's
                                        // tx_finalized rate limiter when our mempool had ΓëÑ20 entries.
                                        check_rate_limit!("general");
                                        let peer_ip_str = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.consensus = Some(Arc::clone(&resources.consensus));
                                        if let Ok(Some(response)) = handler.handle_message(&msg, &context).await {
                                            let _ = self.peer_registry.send_to_peer(&ip_str, response).await;
                                        }
                                    }
                                    NetworkMessage::GetPeers => {
                                        check_rate_limit!("get_peers");

                                        // Use unified message handler
                                        let peer_ip_str = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.peer_manager = Some(Arc::clone(&resources.peer_manager));

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetMasternodes => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::PeersResponse(peers) => {
                                        // Use unified message handler with resources.peer_manager
                                        let peer_ip_str = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip_str, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );
                                        context.peer_manager = Some(Arc::clone(&resources.peer_manager));

                                        let _ = handler.handle_message(&msg, &context).await;

                                        // Log statistics
                                        tracing::debug!("≡ƒôÑ Received PeersResponse from {} with {} peer(s)", peer_addr, peers.len());
                                    }
                                    NetworkMessage::MasternodesResponse(_) => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        let _ = handler.handle_message(&msg, &context).await;
                                    }
                                    NetworkMessage::BlockInventory(_) => {
                                        check_rate_limit!("block");

                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::BlockRequest(_) => {
                                        check_rate_limit!("block");

                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::BlockResponse(block) => {
                                        check_message_size!(MAX_BLOCK_SIZE, "Block");
                                        check_rate_limit!("block");

                                        // SECURITY: Check resources.banlist before processing ANY block
                                        {
                                            let mut bl = resources.banlist.write().await;
                                            if let Some(reason) = bl.is_banned(ip) {
                                                tracing::warn!(
                                                    "≡ƒÜ½ REJECTING BlockResponse from banned peer {}: {}",
                                                    peer_addr, reason
                                                );
                                                continue;
                                            }
                                        }

                                        let block_height = block.header.height;

                                        // Check if we've already seen this block using Bloom filter
                                        let block_height_bytes = block_height.to_le_bytes();
                                        let already_seen = resources.seen_blocks.check_and_insert(&block_height_bytes).await;

                                        if already_seen {
                                            tracing::debug!("≡ƒöü Ignoring duplicate block {} from {}", block_height, peer_addr);
                                            continue;
                                        }

                                        tracing::info!("≡ƒôÑ Received block {} response from {}", block_height, peer_addr);

                                        // Add block to our self.blockchain with fork handling
                                        // Run on blocking thread to keep tokio workers free for RPC/networking
                                        let bc = self.blockchain.clone();
                                        let blk = block.clone();
                                        let result = tokio::task::spawn_blocking(move || {
                                            tokio::runtime::Handle::current().block_on(async {
                                                bc.add_block_with_fork_handling(blk).await
                                            })
                                        }).await;
                                        match result.unwrap_or_else(|e| Err(format!("Block processing panicked: {}", e))) {
                                            Ok(true) => {
                                                tracing::info!("Γ£à Added block {} from {}", block_height, peer_addr);

                                                // GOSSIP: Send inventory to all other connected peers
                                                let msg = NetworkMessage::BlockInventory(block_height);
                                                match resources.broadcast_tx.send(msg) {
                                                    Ok(receivers) => {
                                                        tracing::info!("≡ƒöä Gossiped block {} inventory to {} other peer(s)", block_height, receivers.saturating_sub(1));
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("Failed to gossip block inventory: {}", e);
                                                    }
                                                }
                                            }
                                            Ok(false) => {
                                                tracing::debug!("ΓÅ¡∩╕Å Skipped block {} (already have or invalid)", block_height);
                                            }
                                            Err(e) if e.contains("corrupted") || e.contains("serialization failed") => {
                                                // SECURITY: Corrupted block from peer - severe violation
                                                // Whitelisted peers are operator-trusted infrastructure;
                                                // a corrupted block from one is almost always a software
                                                // bug or version mismatch, not an attack. Log loudly,
                                                // skip the block, but keep the connection.
                                                if is_whitelisted {
                                                    tracing::error!(
                                                        "≡ƒÜ¿ CORRUPTED BLOCK {} from WHITELISTED peer {} ΓÇö skipping block, keeping connection: {}",
                                                        block_height, peer_addr, e
                                                    );
                                                } else {
                                                    tracing::error!(
                                                        "≡ƒÜ¿ CORRUPTED BLOCK {} from {} - recording severe violation: {}",
                                                        block_height, peer_addr, e
                                                    );
                                                    let should_ban = resources.banlist.write().await.record_severe_violation(
                                                        ip,
                                                        &format!("Sent corrupted block {}: {}", block_height, e)
                                                    );
                                                    if should_ban {
                                                        tracing::warn!("≡ƒÜ½ Disconnecting {} for sending corrupted block", peer_addr);
                                                        self.peer_registry.kick_peer(&ip_str).await;
                                                        break; // Exit the message loop to disconnect
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("Γ¥î Failed to add block {}: {}", block_height, e);
                                            }
                                        }
                                    }
                                    NetworkMessage::BlockAnnouncement(block) => {
                                        // Legacy full block announcement (for backward compatibility)
                                        check_message_size!(MAX_BLOCK_SIZE, "Block");
                                        check_rate_limit!("block");

                                        // SECURITY: Check resources.banlist before processing ANY block
                                        {
                                            let mut bl = resources.banlist.write().await;
                                            if let Some(reason) = bl.is_banned(ip) {
                                                tracing::warn!(
                                                    "≡ƒÜ½ REJECTING BlockAnnouncement from banned peer {}: {}",
                                                    peer_addr, reason
                                                );
                                                continue;
                                            }
                                        }

                                        let block_height = block.header.height;

                                        // Check if we've already seen this block using Bloom filter
                                        let block_height_bytes = block_height.to_le_bytes();
                                        let already_seen = resources.seen_blocks.check_and_insert(&block_height_bytes).await;

                                        if already_seen {
                                            tracing::debug!("≡ƒöü Ignoring duplicate block {} from {}", block_height, peer_addr);
                                            continue;
                                        }

                                        tracing::debug!("≡ƒôÑ Received legacy block {} announcement from {}", block_height, peer_addr);

                                        // Add block to our self.blockchain with fork handling
                                        // Run on blocking thread to keep tokio workers free for RPC/networking
                                        let bc = self.blockchain.clone();
                                        let blk = block.clone();
                                        let result = tokio::task::spawn_blocking(move || {
                                            tokio::runtime::Handle::current().block_on(async {
                                                bc.add_block_with_fork_handling(blk).await
                                            })
                                        }).await;
                                        match result.unwrap_or_else(|e| Err(format!("Block processing panicked: {}", e))) {
                                            Ok(true) => {
                                                tracing::info!("Γ£à Added block {} from {}", block_height, peer_addr);

                                                // GOSSIP: Use inventory for efficiency
                                                let msg = NetworkMessage::BlockInventory(block_height);
                                                match resources.broadcast_tx.send(msg) {
                                                    Ok(receivers) => {
                                                        tracing::info!("≡ƒöä Gossiped block {} inventory to {} other peer(s)", block_height, receivers.saturating_sub(1));
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("Failed to gossip block inventory: {}", e);
                                                    }
                                                }
                                            }
                                            Ok(false) => {
                                                tracing::debug!("ΓÅ¡∩╕Å Skipped block {} (already have or fork)", block_height);
                                            }
                                            Err(e) if e.contains("corrupted") || e.contains("serialization failed") => {
                                                // SECURITY: Corrupted block from peer - severe violation
                                                // Whitelisted peers are operator-trusted; never close them.
                                                if is_whitelisted {
                                                    tracing::error!(
                                                        "≡ƒÜ¿ CORRUPTED BLOCK {} from WHITELISTED peer {} (announcement) ΓÇö skipping block, keeping connection: {}",
                                                        block_height, peer_addr, e
                                                    );
                                                } else {
                                                    tracing::error!(
                                                        "≡ƒÜ¿ CORRUPTED BLOCK {} from {} (announcement) - recording severe violation: {}",
                                                        block_height, peer_addr, e
                                                    );
                                                    let should_ban = resources.banlist.write().await.record_severe_violation(
                                                        ip,
                                                        &format!("Sent corrupted block {}: {}", block_height, e)
                                                    );
                                                    if should_ban {
                                                        tracing::warn!("≡ƒÜ½ Disconnecting {} for sending corrupted block", peer_addr);
                                                        self.peer_registry.kick_peer(&ip_str).await;
                                                        break;
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("Failed to add announced block: {}", e);
                                            }
                                        }
                                    }
                                    NetworkMessage::GenesisAnnouncement(block) => {
                                        // Special handling for genesis block announcements
                                        check_message_size!(MAX_BLOCK_SIZE, "GenesisBlock");
                                        check_rate_limit!("genesis");

                                        // Verify this is actually a genesis block
                                        if block.header.height != 0 {
                                            tracing::warn!("ΓÜá∩╕Å  Received GenesisAnnouncement for non-genesis block {} from {}", block.header.height, peer_addr);
                                            continue;
                                        }

                                        // Check if we already have genesis - try to get block at height 0
                                        if self.blockchain.get_block_by_height(0).await.is_ok() {
                                            tracing::debug!("ΓÅ¡∩╕Å Ignoring genesis announcement (already have genesis) from {}", peer_addr);
                                            continue;
                                        }

                                        tracing::info!("≡ƒôª Received genesis announcement from {}", peer_addr);

                                        // Simply verify basic genesis structure
                                        use crate::block::genesis::GenesisBlock;
                                        match GenesisBlock::verify_structure(block) {
                                            Ok(()) => {
                                                tracing::info!("Γ£à Genesis structure validation passed, adding to chain");

                                                // Add genesis to our self.blockchain
                                                match self.blockchain.add_block(block.clone()).await {
                                                    Ok(()) => {
                                                        tracing::info!("Γ£à Genesis block added successfully, hash: {}", hex::encode(&block.hash()[..8]));

                                                        // Broadcast to other peers who might not have it yet
                                                        let msg = NetworkMessage::GenesisAnnouncement(block.clone());
                                                        let _ = resources.broadcast_tx.send(msg);
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Γ¥î Failed to add genesis block: {}", e);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!("ΓÜá∩╕Å  Genesis validation failed: {}", e);
                                            }
                                        }
                                    }
                                    NetworkMessage::RequestGenesis => {
                                        check_rate_limit!("genesis_request");

                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetBlockHash(_) => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::ConsensusQuery { .. } => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetBlockRange { .. } => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::BlocksResponse(_) | NetworkMessage::BlockRangeResponse(_) => {
                                        // Γ£à REFACTORED: Route through unified message_handler.rs
                                        // See: analysis/REFACTORING_ROADMAP.md - Phase 1, Step 1.2 (COMPLETED)

                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        // Handle the message through unified handler
                                        let _ = handler.handle_message(&msg, &context).await;
                                    }
                                    // Health Check Messages
                                    NetworkMessage::Ping { .. } | NetworkMessage::Pong { .. } => {
                                        let rate_ok = {
                                            let mut limiter = rate_limiter.write().await;
                                            let msg_type = if matches!(&msg, NetworkMessage::Ping { .. }) { "ping" } else { "pong" };
                                            limiter.check(msg_type, &ip_str)
                                        };
                                        if !rate_ok {
                                            if matches!(&msg, NetworkMessage::Ping { .. }) {
                                                ping_excess_streak += 1;
                                                tracing::debug!(
                                                    "ΓÜí Ping rate limit exceeded from {} (excess streak: {})",
                                                    peer_addr, ping_excess_streak
                                                );
                                                if ping_excess_streak >= 3 {
                                                    tracing::warn!(
                                                        "≡ƒîè Ping flood from {} (excess streak {}): recording violation",
                                                        peer_addr, ping_excess_streak
                                                    );
                                                    let should_ban = resources.banlist.write().await.record_violation(
                                                        ip,
                                                        "Ping flood: sustained excess pings"
                                                    );
                                                    if let Some(ai) = &self.ai_system {
                                                        ai.attack_detector.record_ping_flood(&ip_str);
                                                    }
                                                    if should_ban {
                                                        tracing::warn!("≡ƒÜ½ Disconnecting {} due to ping flood violations", peer_addr);
                                                        self.peer_registry.kick_peer(&ip_str).await;
                                                        break;
                                                    }
                                                    ping_excess_streak = 0;
                                                }
                                            }
                                            continue;
                                        }
                                        ping_excess_streak = 0;

                                        // Route through unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::TimeVoteRequest { txid, tx_hash_commitment, slot_index, tx } => {
                                        check_message_size!(MAX_VOTE_SIZE, "TimeVoteRequest");
                                        check_rate_limit!("vote");

                                        // Spawn vote processing (non-blocking)
                                        let txid_val = *txid;
                                        let tx_hash_commitment_val = *tx_hash_commitment;
                                        let slot_index_val = *slot_index;
                                        let tx_from_request = tx.clone(); // NEW: Optional TX included in request
                                        let peer_addr_str = peer_addr.to_string();
                                        let ip_str_clone = ip_str.clone();
                                        let consensus_clone = Arc::clone(&resources.consensus);
                                        let peer_registry_clone = Arc::clone(&self.peer_registry);

                                        tokio::spawn(async move {
                                            tracing::info!(
                                                "≡ƒù│∩╕Å  TimeVoteRequest from {} for TX {} (slot {}){}",
                                                peer_addr_str,
                                                hex::encode(txid_val),
                                                slot_index_val,
                                                if tx_from_request.is_some() { " [TX included]" } else { "" }
                                            );

                                            // FIX: Step 1 - Get TX from mempool OR from request
                                            let mut tx_opt = consensus_clone.tx_pool.get_pending(&txid_val);

                                            // If not in mempool but included in request, add it
                                            if tx_opt.is_none() {
                                                if let Some(tx_from_req) = tx_from_request {
                                                    tracing::debug!(
                                                        "≡ƒôÑ TX {} not in mempool, adding from TimeVoteRequest",
                                                        hex::encode(txid_val)
                                                    );

                                                    // Add to pending pool (this also validates basic structure)
                                                    let input_sum: u64 = {
                                                        let mut sum = 0u64;
                                                        for input in &tx_from_req.inputs {
                                                            if let Ok(utxo) = consensus_clone.utxo_manager.get_utxo(&input.previous_output).await {
                                                                sum += utxo.value;
                                                            }
                                                        }
                                                        sum
                                                    };
                                                    let output_sum: u64 = tx_from_req.outputs.iter().map(|o| o.value).sum();
                                                    let fee = input_sum.saturating_sub(output_sum);

                                                    if consensus_clone.tx_pool.add_pending(tx_from_req.clone(), fee).is_ok() {
                                                        tracing::debug!("Γ£à TX {} added to mempool from request", hex::encode(txid_val));
                                                        tx_opt = Some(tx_from_req);
                                                    }
                                                }
                                            }

                                            let decision = if let Some(tx) = tx_opt {
                                                // Step 2: Verify tx_hash_commitment matches actual transaction
                                                let actual_commitment = crate::types::TimeVote::calculate_tx_commitment(&tx);
                                                if actual_commitment != tx_hash_commitment_val {
                                                    tracing::warn!(
                                                        "ΓÜá∩╕Å  TX {} commitment mismatch: expected {:?}, got {:?}",
                                                        hex::encode(txid_val),
                                                        hex::encode(actual_commitment),
                                                        hex::encode(tx_hash_commitment_val)
                                                    );
                                                    crate::types::VoteDecision::Reject
                                                } else {
                                                    // Step 3: Verify UTXOs are available (basic validation)
                                                    match consensus_clone.validate_transaction(&tx).await {
                                                        Ok(_) => {
                                                            tracing::info!("Γ£à TX {} validated successfully for vote", hex::encode(txid_val));
                                                            crate::types::VoteDecision::Accept
                                                        }
                                                        Err(e) => {
                                                            tracing::warn!("ΓÜá∩╕Å  TX {} validation failed: {}", hex::encode(txid_val), e);
                                                            crate::types::VoteDecision::Reject
                                                        }
                                                    }
                                                }
                                            } else {
                                                tracing::debug!("ΓÜá∩╕Å  TX {} not found in mempool and not included in request", hex::encode(txid_val));
                                                crate::types::VoteDecision::Reject
                                            };

                                            // Step 4: Sign TimeVote with our masternode key
                                            let vote_opt = consensus_clone.sign_timevote(
                                                txid_val,
                                                tx_hash_commitment_val,
                                                slot_index_val,
                                                decision,
                                            );

                                            if let Some(vote) = vote_opt {
                                                // Step 5: Send TimeVoteResponse with signed vote
                                                let vote_response = NetworkMessage::TimeVoteResponse { vote };
                                                match peer_registry_clone.send_to_peer(&ip_str_clone, vote_response).await {
                                                    Ok(_) => {
                                                        tracing::info!(
                                                            "Γ£à TimeVoteResponse sent to {} for TX {} (decision: {:?})",
                                                            ip_str_clone,
                                                            hex::encode(txid_val),
                                                            decision
                                                        );
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            "Γ¥î Failed to send TimeVoteResponse to {} for TX {}: {}",
                                                            ip_str_clone,
                                                            hex::encode(txid_val),
                                                            e
                                                        );
                                                    }
                                                }
                                            } else {
                                                tracing::warn!(
                                                    "ΓÜá∩╕Å TimeVote signing skipped for TX {} (not a masternode or identity not set)",
                                                    hex::encode(txid_val)
                                                );
                                            }
                                        });
                                    }
                                    NetworkMessage::TimeVoteResponse { vote } => {
                                        check_message_size!(MAX_VOTE_SIZE, "TimeVoteResponse");
                                        check_rate_limit!("vote");

                                        // Received a signed TimeVote from a peer
                                        tracing::info!(
                                            "≡ƒôÑ TimeVoteResponse from {} for TX {} (decision: {:?}, weight: {})",
                                            peer_addr,
                                            hex::encode(vote.txid),
                                            vote.decision,
                                            vote.voter_weight
                                        );

                                        let txid = vote.txid;
                                        let vote_clone = vote.clone();
                                        let consensus_clone = Arc::clone(&resources.consensus);
                                        let tx_pool = Arc::clone(&resources.consensus.tx_pool);

                                        // Spawn finality check (non-blocking)
                                        tokio::spawn(async move {
                                            // Step 1: Accumulate the vote
                                            let accumulated_weight = match consensus_clone.timevote.accumulate_timevote(vote_clone.clone()) {
                                                Ok(weight) => weight,
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "Failed to accumulate vote for TX {}: {}",
                                                        hex::encode(txid),
                                                        e
                                                    );
                                                    return;
                                                }
                                            };

                                            tracing::info!(
                                                "Vote accumulated for TX {}, total weight: {}",
                                                hex::encode(txid),
                                                accumulated_weight
                                            );

                                            // Step 2: Check if finality threshold reached (67% BFT-safe majority)
                                            let validators = consensus_clone.timevote.get_validators();
                                            let total_avs_weight: u64 = validators.iter().map(|v| v.weight).sum();
                                            let finality_threshold = ((total_avs_weight as f64) * 0.67).ceil() as u64;

                                            tracing::info!(
                                                "Finality check for TX {}: accumulated={}, threshold={} (67% of {})",
                                                hex::encode(txid),
                                                accumulated_weight,
                                                finality_threshold,
                                                total_avs_weight
                                            );

                                            // Step 3: If threshold met, finalize transaction
                                            if accumulated_weight >= finality_threshold {
                                                tracing::info!(
                                                    "≡ƒÄë TX {} reached finality threshold! ({} >= {})",
                                                    hex::encode(txid),
                                                    accumulated_weight,
                                                    finality_threshold
                                                );

                                                // FIX: Use atomic finalization guard to prevent race conditions
                                                // Multiple concurrent votes may all try to finalize - only first succeeds
                                                use dashmap::mapref::entry::Entry;
                                                match consensus_clone.timevote.finalized_txs.entry(txid) {
                                                    Entry::Vacant(e) => {
                                                        // We're the first to finalize - claim it
                                                        e.insert((crate::consensus::Preference::Accept, std::time::Instant::now()));

                                                        tracing::info!(
                                                            "≡ƒöÆ Acquired finalization lock for TX {}",
                                                            hex::encode(txid)
                                                        );

                                                        // Move transaction from pending to finalized
                                                        let tx_data = tx_pool.get_pending(&txid);
                                                        if tx_pool.finalize_transaction(txid) {
                                                            tracing::info!(
                                                                "Γ£à TX {} moved to finalized pool",
                                                                hex::encode(txid)
                                                            );

                                                            // Transition input UTXOs and create output UTXOs
                                                            if let Some(ref tx) = tx_data {
                                                                for input in &tx.inputs {
                                                                    let new_state = crate::types::UTXOState::SpentFinalized {
                                                                        txid,
                                                                        finalized_at: chrono::Utc::now().timestamp(),
                                                                        votes: 0,
                                                                    };
                                                                    consensus_clone.utxo_manager.update_state(&input.previous_output, new_state);
                                                                }
                                                                for (idx, output) in tx.outputs.iter().enumerate() {
                                                                    let outpoint = crate::types::OutPoint {
                                                                        txid,
                                                                        vout: idx as u32,
                                                                    };
                                                                    let utxo = crate::types::UTXO {
                                                                        outpoint: outpoint.clone(),
                                                                        value: output.value,
                                                                        script_pubkey: output.script_pubkey.clone(),
                                                                        address: String::from_utf8(output.script_pubkey.clone())
                                                                            .unwrap_or_default(),
                                                                    masternode_key: None,
                                                                    };
                                                                    if let Err(e) = consensus_clone.utxo_manager.add_utxo(utxo).await {
                                                                        tracing::warn!("Failed to add output UTXO vout={}: {}", idx, e);
                                                                    }
                                                                    consensus_clone.utxo_manager.update_state(&outpoint, crate::types::UTXOState::Unspent);
                                                                }
                                                            }

                                                            // Record finalization weight
                                                            consensus_clone.timevote.record_finalization(txid, accumulated_weight);

                                                            // Notify WS subscribers about finalized transaction
                                                            consensus_clone.signal_tx_finalized(txid);

                                                            // Assemble TimeProof certificate
                                                            match consensus_clone.timevote.assemble_timeproof(txid) {
                                                                Ok(timeproof) => {
                                                                    tracing::info!(
                                                                        "≡ƒô£ TimeProof assembled for TX {} with {} votes",
                                                                        hex::encode(txid),
                                                                        timeproof.votes.len()
                                                                    );

                                                                    // Store TimeProof in finality_proof_manager
                                                                    if let Err(e) = consensus_clone.finality_proof_mgr.store_timeproof(timeproof.clone()) {
                                                                        tracing::error!(
                                                                            "Γ¥î Failed to store TimeProof for TX {}: {}",
                                                                            hex::encode(txid),
                                                                            e
                                                                        );
                                                                    }

                                                                    // Broadcast TimeProof to network (Task 2.5)
                                                                    consensus_clone.broadcast_timeproof(timeproof).await;
                                                                }
                                                                Err(e) => {
                                                                    tracing::error!(
                                                                        "Γ¥î Failed to assemble TimeProof for TX {}: {}",
                                                                        hex::encode(txid),
                                                                        e
                                                                    );
                                                                }
                                                            }
                                                        } else {
                                                            tracing::warn!(
                                                                "ΓÜá∩╕Å  Failed to finalize TX {} - not found in pending pool",
                                                                hex::encode(txid)
                                                            );
                                                        }
                                                    }
                                                    Entry::Occupied(_) => {
                                                        // Another task already finalized this TX - skip
                                                        tracing::debug!(
                                                            "TX {} already finalized by another task",
                                                            hex::encode(txid)
                                                        );
                                                    }
                                                }
                                            }
                                        });
                                    }
                                    NetworkMessage::TimeProofBroadcast { proof } => {
                                        check_message_size!(MAX_VOTE_SIZE, "TimeProofBroadcast");
                                        check_rate_limit!("vote");

                                        // Received TimeProof certificate from peer
                                        let proof_clone = proof.clone();
                                        let consensus_clone = Arc::clone(&resources.consensus);
                                        let peer_addr_str = peer_addr.to_string();

                                        // Spawn verification (non-blocking)
                                        tokio::spawn(async move {
                                            tracing::info!(
                                                "≡ƒô£ Received TimeProof from {} for TX {} with {} votes",
                                                peer_addr_str,
                                                hex::encode(proof_clone.txid),
                                                proof_clone.votes.len()
                                            );

                                            // Verify TimeProof using resources.consensus engine's verification method
                                            match consensus_clone.timevote.verify_timeproof(&proof_clone) {
                                                Ok(_accumulated_weight) => {
                                                    tracing::info!(
                                                        "Γ£à TimeProof verified for TX {}",
                                                        hex::encode(proof_clone.txid)
                                                    );

                                                    // Store verified TimeProof
                                                    if let Err(e) = consensus_clone.finality_proof_mgr.store_timeproof(proof_clone.clone()) {
                                                        tracing::error!(
                                                            "Γ¥î Failed to store TimeProof for TX {}: {}",
                                                            hex::encode(proof_clone.txid),
                                                            e
                                                        );
                                                    } else {
                                                        tracing::info!(
                                                            "≡ƒÆ╛ TimeProof stored for TX {}",
                                                            hex::encode(proof_clone.txid)
                                                        );
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "ΓÜá∩╕Å  Invalid TimeProof from {}: {}",
                                                        peer_addr_str,
                                                        e
                                                    );
                                                }
                                            }
                                        });
                                    }
                                    NetworkMessage::TransactionVoteRequest { .. }
                                    | NetworkMessage::TransactionVoteResponse { .. } => {
                                        // Deprecated legacy vote protocol ΓÇö superseded by TimeVoteRequest/Response.
                                        // These are no-ops: the response never updated resources.consensus state.
                                        check_rate_limit!("vote");
                                    }
                                    NetworkMessage::FinalityVoteBroadcast { vote } => {
                                        check_message_size!(MAX_VOTE_SIZE, "FinalityVote");
                                        check_rate_limit!("vote");

                                        // Received a finality vote from a peer
                                        tracing::debug!("≡ƒôÑ Finality vote from {} for TX {}", peer_addr, hex::encode(vote.txid));

                                        // Accumulate the finality vote in resources.consensus
                                        if let Err(e) = resources.consensus.timevote.accumulate_finality_vote(vote.clone()) {
                                            tracing::warn!("Failed to accumulate finality vote from {}: {}", peer_addr, e);
                                        } else {
                                            tracing::debug!("Γ£à Finality vote recorded from {}", peer_addr);
                                        }
                                    }
                                    NetworkMessage::TimeLockBlockProposal { .. }
                                    | NetworkMessage::TimeVotePrepare { .. }
                                    | NetworkMessage::TimeVotePrecommit { .. } => {
                                        // SECURITY: Check resources.banlist before processing ANY resources.consensus messages
                                        {
                                            let mut bl = resources.banlist.write().await;
                                            if let Some(reason) = bl.is_banned(ip) {
                                                tracing::warn!(
                                                    "≡ƒÜ½ REJECTING TimeLock message from banned peer {}: {}",
                                                    peer_addr, reason
                                                );
                                                continue;
                                            }
                                        }

                                        // Use unified message handler for TimeLock messages
                                        let handler = MessageHandler::new(ip_str.clone(), ConnectionDirection::Inbound);
                                        // Get local masternode address for vote identity
                                        let local_mn_addr = self.masternode_registry.get_local_address().await;
                                        let context = MessageContext::with_consensus(
                                            self.blockchain.clone(),
                                            self.peer_registry.clone(),
                                            self.masternode_registry.clone(),
                                            resources.consensus.clone(),
                                            resources.block_cache.clone(),
                                            resources.broadcast_tx.clone(),
                                            local_mn_addr,
                                        ).with_banlist(Arc::clone(&resources.banlist));

                                        if let Err(e) = handler.handle_message(&msg, &context).await {
                                            tracing::warn!("[Inbound] Error handling TimeLock message from {}: {}", peer_addr, e);
                                        }
                                    }
                                    NetworkMessage::GetChainWork => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::GetChainWorkAt(_) => {
                                        // Use unified message handler
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => { let _ = self.peer_registry.send_to_peer(&ip_str, response).await; }
                                            Err(e) if e.contains("DISCONNECT:") => { tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e); break; }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::ChainWorkResponse { height, tip_hash, cumulative_work } => {
                                        // Handle response - check if peer has better chain and potentially trigger reorg
                                        let _our_height = self.blockchain.get_height();

                                        if self.blockchain.should_switch_by_work(*cumulative_work, *height, tip_hash, Some(&ip_str)).await {
                                            tracing::info!(
                                                "≡ƒôè Peer {} has better chain, requesting blocks",
                                                peer_addr
                                            );

                                            // Check for fork and request the first batch of missing blocks.
                                            // Cap to 50 blocks to stay well within the 16MB frame limit;
                                            // subsequent batches will be fetched by the normal sync path.
                                            if let Some(fork_height) = self.blockchain.detect_fork(*height, *tip_hash).await {
                                                tracing::warn!(
                                                    "≡ƒöÇ Fork detected at height {} with {}, requesting blocks",
                                                    fork_height, peer_addr
                                                );

                                                let batch_end = (*height).min(fork_height + 49);
                                                let request = NetworkMessage::GetBlockRange {
                                                    start_height: fork_height,
                                                    end_height: batch_end,
                                                };
                                                let _ = self.peer_registry.send_to_peer(&ip_str, request).await;
                                            }
                                        }
                                    }
                                    NetworkMessage::ChainWorkAtResponse { .. }
                                    | NetworkMessage::BlockHashResponse { .. } => {
                                        // Handle via response system - dispatched to waiting oneshot channels
                                        self.peer_registry.handle_response(&ip_str, msg).await;
                                    }
                                    NetworkMessage::ChainTipResponse { .. } => {
                                        // Route through unified message handler to update peer_chain_tips cache
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        match handler.handle_message(&msg, &context).await {
                                            Err(e) if e.contains("DISCONNECT:") => {
                                                tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e);
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                    NetworkMessage::MasternodeStatusGossip { .. } => {
                                        // Handle gossip via unified message handler
                                        let handler = MessageHandler::new(ip_str.clone(), ConnectionDirection::Inbound);
                                        let context = MessageContext::minimal(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        );

                                        if let Err(e) = handler.handle_message(&msg, &context).await {
                                            tracing::warn!("[Inbound] Error handling gossip from {}: {}", peer_addr, e);
                                        }
                                    }
                                    _ => {
                                        // Fallback: delegate any unhandled message types to MessageHandler
                                        // with full resources.consensus context so messages like MempoolSyncResponse
                                        // (which need resources.consensus to add transactions) are processed correctly.
                                        let peer_ip = peer_addr.split(':').next().unwrap_or("").to_string();
                                        let handler = MessageHandler::new(peer_ip, ConnectionDirection::Inbound);
                                        let mut context = MessageContext::from_registry(
                                            Arc::clone(&self.blockchain),
                                            Arc::clone(&self.peer_registry),
                                            Arc::clone(&self.masternode_registry),
                                        ).await;
                                        context.banlist = Some(Arc::clone(&resources.banlist));
                                        if let Some(ref ai) = self.ai_system {
                                            context.ai_system = Some(Arc::clone(ai));
                                        }

                                        match handler.handle_message(&msg, &context).await {
                                            Ok(Some(response)) => {
                                                let _ = self.peer_registry.send_to_peer(&ip_str, response).await;
                                            }
                                            Err(e) if e.contains("DISCONNECT:") => {
                                                tracing::warn!("≡ƒöî Disconnecting {} ΓÇö {}", peer_addr, e);
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                    }

                result = notifier.recv() => {
                    match result {
                        Ok(msg) => {
                            // Log what we're broadcasting
                            match &msg {
                                NetworkMessage::BlockAnnouncement(block) => {
                                    tracing::debug!("≡ƒôñ Sending block {} to peer {}", block.header.height, peer_addr);
                                }
                                NetworkMessage::BlockInventory(height) => {
                                    tracing::debug!("≡ƒôñ Sending block {} inventory to peer {}", height, peer_addr);
                                }
                                _ => {
                                    tracing::debug!("≡ƒôñ Sending message to peer {}", peer_addr);
                                }
                            }

                            let _ = self.peer_registry.send_to_peer(&ip_str, msg).await;
                        }
                        Err(_) => break,
                    }
                }

                // Close connections that complete TLS but never send a Handshake message.
                // The guard disables this arm after the handshake succeeds so there is no
                // ongoing per-iteration overhead once the connection is fully established.
                _ = &mut handshake_timeout, if !handshake_done => {
                    tracing::warn!(
                        "ΓÅ░ Pre-handshake timeout from {} ΓÇö no handshake received within 10s, closing",
                        peer_addr
                    );
                    resources.banlist.write().await.record_violation(
                        ip,
                        "Pre-handshake timeout: no handshake message within 10s",
                    );
                    // Feed into AI: coordinated slow-loris patterns (many IPs holding
                    // slots open without sending) accumulate here and trigger BanSubnet.
                    if let Some(ref ai) = self.ai_system {
                        ai.attack_detector.record_pre_handshake_violation(&ip_str);
                    }
                    break;
                }
            }
        }

        // Cleanup: mark inbound connection as disconnected in BOTH managers
        self.connection_manager.mark_inbound_disconnected(&ip_str);
        self.peer_registry.unregister_peer(&ip_str).await;

        // Mark masternode as inactive only if the handshake completed.
        // Connections that never completed the version exchange (e.g., old
        // software that sends messages before the handshake) must not trigger
        // registry changes: the peer never identified itself on this connection,
        // so there is nothing meaningful to update.  Without this guard, inbound
        // pre-handshake failures from old-software peers cause their previously
        // registered entry (which may be a paid-tier node) to be removed as a
        // "transient Free-tier" node, creating continuous reconnection churn.
        if handshake_done {
            if let Err(e) = self
                .masternode_registry
                .mark_inactive_on_disconnect(&ip_str)
                .await
            {
                tracing::debug!("Note: {} is not a registered masternode ({})", ip_str, e);
            }
            // Notify AI detector of masternode disconnect for synchronized cycling detection (AV3).
            // If ΓëÑ5 nodes from the same /24 subnet disconnect within 30s, the AI will recommend
            // BanSubnet, which the enforcement loop applies on its next 30s tick.
            if let Some(ref ai) = self.ai_system {
                ai.attack_detector.record_synchronized_disconnect(&ip_str);
            }
        }

        tracing::info!("≡ƒöî Peer {} disconnected", peer_addr);

        Ok(())
    }
}
