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
use crate::network::peer_connection::{MessageLoopConfig, PeerConnection};
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::network::rate_limiter::RateLimiter;
use crate::network::tls::TlsConfig;

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
        let _rate_limiter = Arc::new(RwLock::new(RateLimiter::new()));

        // Get WebSocket tx event sender for real-time wallet notifications
        let _ws_tx_event_sender = self.peer_registry.get_tx_event_sender().await;

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
            tokio::sync::mpsc::unbounded_channel::<Result<Option<NetworkMessage>, String>>();
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
                                    let _ = msg_read_tx.send(Err(
                                        "Message flood detected: pre-channel gate triggered"
                                            .to_string(),
                                    ));
                                    break;
                                }
                                continue; // soft drop
                            }
                            if msg_read_tx.send(result).is_err() {
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
                                    .to_string()));
                            break;
                        }
                        continue; // soft drop
                    }
                    if msg_read_tx.send(result).is_err() {
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

        // Per-connection UTXO lock flood counter: tracks how many UTXOStateUpdate (Locked)
        // messages this peer has sent for each TX.  A legitimate TX with N inputs produces
        // exactly N lock messages ΓÇö an attacker who sends far more is DoS-flooding us.
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
                                            break;
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

        // After the pre-handshake loop, hand off to the unified message loop.
        if handshake_done {
            let remote_port = peer_addr
                .split(':')
                .next_back()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);
            if let Ok(peer_conn) = PeerConnection::new_inbound(
                ip_str.clone(),
                remote_port,
                0,
                is_whitelisted,
                self.network_type,
                writer_tx,
                msg_read_rx,
            ) {
                let mut loop_config = MessageLoopConfig::new(self.peer_registry.clone())
                    .with_masternode_registry(self.masternode_registry.clone())
                    .with_blockchain(self.blockchain.clone());
                if let Some(ref banlist) = self.banlist {
                    loop_config = loop_config.with_banlist(banlist.clone());
                }
                if let (_, _, Some(broadcast_tx)) =
                    self.peer_registry.get_timelock_resources().await
                {
                    loop_config = loop_config.with_broadcast_rx(broadcast_tx.subscribe());
                }
                if let Some(ref ai) = self.ai_system {
                    loop_config = loop_config.with_ai_system(ai.clone());
                }
                let rl = Arc::new(RwLock::new(RateLimiter::new()));
                loop_config = loop_config.with_rate_limiter(rl);
                let _ = peer_conn.run_message_loop_unified(loop_config).await;
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
