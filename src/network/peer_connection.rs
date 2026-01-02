//! Peer Connection Management
//! Handles individual peer connections and message routing.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Instant};
use tracing::{debug, error, info, warn};

use crate::block::types::Block;
use crate::blockchain::Blockchain;
use crate::network::message::NetworkMessage;
use crate::network::message_handler::{ConnectionDirection, MessageContext, MessageHandler};

/// State for tracking ping/pong health
#[derive(Debug)]
struct PingState {
    last_ping_sent: Option<Instant>,
    last_pong_received: Option<Instant>,
    pending_pings: Vec<(u64, Instant)>, // (nonce, sent_time)
    missed_pongs: u32,
}

/// Fork resolution attempt tracker
#[derive(Debug, Clone)]
struct ForkResolutionAttempt {
    fork_height: u64,
    attempt_count: u32,
    last_attempt: std::time::Instant,
    common_ancestor: Option<u64>,
    peer_height: u64,
}

impl ForkResolutionAttempt {
    fn new(fork_height: u64, peer_height: u64) -> Self {
        Self {
            fork_height,
            attempt_count: 1,
            last_attempt: std::time::Instant::now(),
            common_ancestor: None,
            peer_height,
        }
    }

    fn is_same_fork(&self, fork_height: u64, peer_height: u64) -> bool {
        // Consider it the same fork if heights are within 10 blocks
        (self.fork_height as i64 - fork_height as i64).abs() <= 10
            && (self.peer_height as i64 - peer_height as i64).abs() <= 10
    }

    fn should_give_up(&self) -> bool {
        // Only give up if we've searched back more than 2000 blocks (checked elsewhere)
        // or if we've been trying for more than 5 minutes
        let elapsed = self.last_attempt.elapsed();
        elapsed.as_secs() > 300 // 5 minutes
    }

    fn increment(&mut self) {
        self.attempt_count += 1;
        self.last_attempt = std::time::Instant::now();
    }
}

impl PingState {
    fn new() -> Self {
        Self {
            last_ping_sent: None,
            last_pong_received: None,
            pending_pings: Vec::new(),
            missed_pongs: 0,
        }
    }

    fn record_ping_sent(&mut self, nonce: u64) {
        let now = Instant::now();
        self.last_ping_sent = Some(now);
        self.pending_pings.push((nonce, now));

        // Keep only last 5 pings
        if self.pending_pings.len() > 5 {
            self.pending_pings.remove(0);
        }
    }

    fn record_pong_received(&mut self, nonce: u64) -> bool {
        self.last_pong_received = Some(Instant::now());

        // Find and remove the matching ping
        if let Some(pos) = self.pending_pings.iter().position(|(n, _)| *n == nonce) {
            self.pending_pings.remove(pos);
            self.missed_pongs = 0; // Reset counter on successful pong
            true
        } else {
            warn!("⚠️ Received pong for unknown nonce: {}", nonce);
            false
        }
    }

    fn check_timeout(&mut self, max_missed: u32, timeout_duration: Duration) -> bool {
        let now = Instant::now();

        // Check for expired pings
        let mut expired_count = 0;
        self.pending_pings.retain(|(_, sent_time)| {
            if now.duration_since(*sent_time) > timeout_duration {
                expired_count += 1;
                false
            } else {
                true
            }
        });

        if expired_count > 0 {
            self.missed_pongs += expired_count;
            debug!(
                "⏰ {} ping(s) expired, total missed: {}/{}",
                expired_count, self.missed_pongs, max_missed
            );
        }

        self.missed_pongs >= max_missed
    }
}

/// Unified peer connection handling both inbound and outbound connections
pub struct PeerConnection {
    /// Peer's IP address (identity)
    peer_ip: String,

    /// Connection direction
    direction: ConnectionDirection,

    /// TCP reader
    reader: BufReader<OwnedReadHalf>,

    /// TCP writer (shared for concurrent writes)
    writer: Arc<Mutex<BufWriter<OwnedWriteHalf>>>,

    /// Ping/pong state
    ping_state: Arc<RwLock<PingState>>,

    /// Invalid/skipped block counter (for fork detection)
    invalid_block_count: Arc<RwLock<u32>>,

    /// Local listening port (for logging)
    #[allow(dead_code)]
    local_port: u16,

    /// Remote port for this connection (ephemeral)
    remote_port: u16,

    /// Last known height of the peer (for fork resolution)
    peer_height: Arc<RwLock<Option<u64>>>,

    /// Fork resolution attempt tracker
    fork_resolution_tracker: Arc<RwLock<Option<ForkResolutionAttempt>>>,

    /// Fork loop detection: track consecutive fork detections at same height
    fork_loop_tracker: Arc<RwLock<Option<(u64, u32, std::time::Instant)>>>, // (height, count, last_seen)
}

impl PeerConnection {
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    const TIMEOUT_CHECK_INTERVAL: Duration = Duration::from_secs(10);
    const PONG_TIMEOUT: Duration = Duration::from_secs(90);
    const MAX_MISSED_PONGS: u32 = 3;

    /// Create a new outbound connection to a peer
    pub async fn new_outbound(peer_ip: String, port: u16) -> Result<Self, String> {
        let addr = format!("{}:{}", peer_ip, port);

        info!("🔗 [OUTBOUND] Connecting to {}", addr);

        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("Failed to connect to {}: {}", addr, e))?;

        let remote_addr = stream
            .peer_addr()
            .map_err(|e| format!("Failed to get peer address: {}", e))?;

        let local_addr = stream
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?;

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            peer_ip,
            direction: ConnectionDirection::Outbound,
            reader: BufReader::with_capacity(1024 * 1024, read_half), // 1MB buffer for large block responses
            writer: Arc::new(Mutex::new(BufWriter::with_capacity(
                2 * 1024 * 1024,
                write_half,
            ))), // 2MB buffer for large block sends
            ping_state: Arc::new(RwLock::new(PingState::new())),
            invalid_block_count: Arc::new(RwLock::new(0)),
            peer_height: Arc::new(RwLock::new(None)),
            fork_resolution_tracker: Arc::new(RwLock::new(None)),
            fork_loop_tracker: Arc::new(RwLock::new(None)),
            local_port: local_addr.port(),
            remote_port: remote_addr.port(),
        })
    }

    /// Create a new inbound connection from a peer
    #[allow(dead_code)]
    pub async fn new_inbound(stream: TcpStream) -> Result<Self, String> {
        let peer_addr = stream
            .peer_addr()
            .map_err(|e| format!("Failed to get peer address: {}", e))?;

        let local_addr = stream
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?;

        let peer_ip = peer_addr.ip().to_string();

        info!("🔗 [Inbound] Accepted connection from {}", peer_addr);

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            peer_ip,
            direction: ConnectionDirection::Inbound,
            reader: BufReader::with_capacity(1024 * 1024, read_half), // 1MB buffer for large block responses
            writer: Arc::new(Mutex::new(BufWriter::with_capacity(
                2 * 1024 * 1024,
                write_half,
            ))), // 2MB buffer for large block sends
            ping_state: Arc::new(RwLock::new(PingState::new())),
            invalid_block_count: Arc::new(RwLock::new(0)),
            peer_height: Arc::new(RwLock::new(None)),
            fork_resolution_tracker: Arc::new(RwLock::new(None)),
            fork_loop_tracker: Arc::new(RwLock::new(None)),
            local_port: local_addr.port(),
            remote_port: peer_addr.port(),
        })
    }

    /// Get peer IP (identity)
    pub fn peer_ip(&self) -> &str {
        &self.peer_ip
    }

    /// Get connection direction
    #[allow(dead_code)]
    pub fn direction(&self) -> ConnectionDirection {
        self.direction
    }

    /// Get remote port for this connection
    #[allow(dead_code)]
    pub fn remote_port(&self) -> u16 {
        self.remote_port
    }

    /// Get peer's reported blockchain height
    pub async fn get_peer_height(&self) -> Option<u64> {
        *self.peer_height.read().await
    }

    /// Get a clone of the shared writer for registration in peer registry
    pub fn shared_writer(&self) -> Arc<Mutex<BufWriter<OwnedWriteHalf>>> {
        self.writer.clone()
    }

    /// Send a message to the peer
    async fn send_message(&self, message: &NetworkMessage) -> Result<(), String> {
        let mut writer = self.writer.lock().await;

        let msg_json = serde_json::to_string(message)
            .map_err(|e| format!("Failed to serialize message: {}", e))?;

        writer
            .write_all(format!("{}\n", msg_json).as_bytes())
            .await
            .map_err(|e| format!("Failed to write message: {}", e))?;

        writer
            .flush()
            .await
            .map_err(|e| format!("Failed to flush: {}", e))?;

        Ok(())
    }

    /// Send a ping to the peer
    async fn send_ping(&self) -> Result<(), String> {
        let nonce = rand::random::<u64>();
        let timestamp = chrono::Utc::now().timestamp();

        {
            let mut state = self.ping_state.write().await;
            state.record_ping_sent(nonce);
        }

        info!(
            "📤 [{:?}] Sent ping to {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        self.send_message(&NetworkMessage::Ping { nonce, timestamp })
            .await
    }

    /// Handle received ping
    async fn handle_ping(&self, nonce: u64, _timestamp: i64) -> Result<(), String> {
        info!(
            "📨 [{:?}] Received ping from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        let timestamp = chrono::Utc::now().timestamp();
        self.send_message(&NetworkMessage::Pong { nonce, timestamp })
            .await?;

        info!(
            "✅ [{:?}] Sent pong to {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        Ok(())
    }

    /// Handle received pong
    async fn handle_pong(&self, nonce: u64, _timestamp: i64) -> Result<(), String> {
        info!(
            "📨 [{:?}] Received pong from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        let mut state = self.ping_state.write().await;

        info!(
            "📊 [{:?}] Before pong: {} pending pings, {} missed",
            self.direction,
            state.pending_pings.len(),
            state.missed_pongs
        );

        if state.record_pong_received(nonce) {
            info!(
                "✅ [{:?}] Pong MATCHED for {} (nonce: {}), {} pending pings remain",
                self.direction,
                self.peer_ip,
                nonce,
                state.pending_pings.len()
            );
            Ok(())
        } else {
            warn!(
                "⚠️ [{:?}] Pong NOT MATCHED from {} (nonce: {}), pending: {:?}",
                self.direction,
                self.peer_ip,
                nonce,
                state
                    .pending_pings
                    .iter()
                    .map(|(n, _)| n)
                    .collect::<Vec<_>>()
            );
            Ok(())
        }
    }

    /// Check if connection should be closed due to timeout
    async fn should_disconnect(
        &self,
        peer_registry: &crate::network::peer_connection_registry::PeerConnectionRegistry,
    ) -> bool {
        let mut state = self.ping_state.write().await;

        if state.check_timeout(Self::MAX_MISSED_PONGS, Self::PONG_TIMEOUT) {
            // Check if this is a whitelisted peer before disconnecting
            let is_whitelisted = peer_registry.is_whitelisted(&self.peer_ip).await;

            if is_whitelisted {
                warn!(
                    "⚠️ [{:?}] Whitelisted peer {} unresponsive after {} missed pongs - resetting counter but keeping connection",
                    self.direction, self.peer_ip, state.missed_pongs
                );
                // Reset the missed pongs counter for whitelisted peers
                // Don't clear pending_pings - let them expire naturally to avoid mismatches
                state.missed_pongs = 0;
                false
            } else {
                warn!(
                    "⚠️ [{:?}] Peer {} unresponsive after {} missed pongs",
                    self.direction, self.peer_ip, state.missed_pongs
                );
                true
            }
        } else {
            false
        }
    }

    /// Run the unified message loop for this connection with broadcast channel integration
    pub async fn run_message_loop_with_registry(
        mut self,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) -> Result<(), String> {
        let mut ping_interval = interval(Self::PING_INTERVAL);
        let mut timeout_check = interval(Self::TIMEOUT_CHECK_INTERVAL);
        let mut buffer = String::new();

        info!(
            "🔄 [{:?}] Starting message loop for {} (port: {})",
            self.direction, self.peer_ip, self.remote_port
        );

        // Register this outbound connection in the peer registry so sync can reach it
        peer_registry
            .register_peer_shared(self.peer_ip.clone(), self.shared_writer())
            .await;
        info!(
            "📝 [{:?}] Registered {} in PeerConnectionRegistry for sync",
            self.direction, self.peer_ip
        );

        // Send initial handshake (required by protocol)
        let handshake = NetworkMessage::Handshake {
            magic: *b"TIME",
            protocol_version: 1,
            network: "mainnet".to_string(),
        };

        if let Err(e) = self.send_message(&handshake).await {
            error!(
                "❌ [{:?}] Failed to send handshake to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        info!(
            "🤝 [{:?}] Sent handshake to {}",
            self.direction, self.peer_ip
        );

        // Send initial ping
        if let Err(e) = self.send_ping().await {
            error!(
                "❌ [{:?}] Failed to send initial ping to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        loop {
            tokio::select! {
                // Receive messages from peer
                result = self.reader.read_line(&mut buffer) => {
                    match result {
                        Ok(0) => {
                            info!("🔌 [{:?}] Connection to {} closed by peer (EOF)",
                                  self.direction, self.peer_ip);
                            break;
                        }
                        Ok(_) => {
                            if let Err(e) = self.handle_message_with_registry(&buffer, &peer_registry).await {
                                warn!("⚠️ [{:?}] Error handling message from {}: {}",
                                      self.direction, self.peer_ip, e);
                            }
                            buffer.clear();
                        }
                        Err(e) => {
                            error!("❌ [{:?}] Error reading from {}: {}",
                                   self.direction, self.peer_ip, e);
                            break;
                        }
                    }
                }

                // Send periodic pings
                _ = ping_interval.tick() => {
                    if let Err(e) = self.send_ping().await {
                        error!("❌ [{:?}] Failed to send ping to {}: {}",
                               self.direction, self.peer_ip, e);
                        break;
                    }
                }

                // Check for timeout
                _ = timeout_check.tick() => {
                    if self.should_disconnect(&peer_registry).await {
                        error!("❌ [{:?}] Disconnecting {} due to timeout",
                               self.direction, self.peer_ip);
                        break;
                    }
                }
            }
        }

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Run the unified message loop with masternode registry integration
    pub async fn run_message_loop_with_registry_and_masternode(
        mut self,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    ) -> Result<(), String> {
        let mut ping_interval = interval(Self::PING_INTERVAL);
        let mut timeout_check = interval(Self::TIMEOUT_CHECK_INTERVAL);
        let mut buffer = String::new();

        info!(
            "🔄 [{:?}] Starting message loop for {} (port: {})",
            self.direction, self.peer_ip, self.remote_port
        );

        // Register this outbound connection in the peer registry so sync can reach it
        peer_registry
            .register_peer_shared(self.peer_ip.clone(), self.shared_writer())
            .await;
        info!(
            "📝 [{:?}] Registered {} in PeerConnectionRegistry for sync",
            self.direction, self.peer_ip
        );

        // Send initial handshake (required by protocol)
        let handshake = NetworkMessage::Handshake {
            magic: *b"TIME",
            protocol_version: 1,
            network: "mainnet".to_string(),
        };

        if let Err(e) = self.send_message(&handshake).await {
            error!(
                "❌ [{:?}] Failed to send handshake to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        info!(
            "🤝 [{:?}] Sent handshake to {}",
            self.direction, self.peer_ip
        );

        // Send initial ping
        if let Err(e) = self.send_ping().await {
            error!(
                "❌ [{:?}] Failed to send initial ping to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        loop {
            tokio::select! {
                // Receive messages from peer
                result = self.reader.read_line(&mut buffer) => {
                    match result {
                        Ok(0) => {
                            info!("🔌 [{:?}] Connection to {} closed by peer (EOF)",
                                  self.direction, self.peer_ip);
                            break;
                        }
                        Ok(_) => {
                            if let Err(e) = self.handle_message_with_masternode_registry(&buffer, &peer_registry, &masternode_registry).await {
                                warn!("⚠️ [{:?}] Error handling message from {}: {}",
                                      self.direction, self.peer_ip, e);
                            }
                            buffer.clear();
                        }
                        Err(e) => {
                            error!("❌ [{:?}] Error reading from {}: {}",
                                   self.direction, self.peer_ip, e);
                            break;
                        }
                    }
                }

                // Send periodic pings
                _ = ping_interval.tick() => {
                    if let Err(e) = self.send_ping().await {
                        error!("❌ [{:?}] Failed to send ping to {}: {}",
                               self.direction, self.peer_ip, e);
                        break;
                    }
                }

                // Check for timeout
                _ = timeout_check.tick() => {
                    if self.should_disconnect(&peer_registry).await {
                        error!("❌ [{:?}] Disconnecting {} due to timeout",
                               self.direction, self.peer_ip);
                        break;
                    }
                }
            }
        }

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Run the unified message loop with masternode registry AND blockchain integration
    /// This is used for outbound connections that need to receive block sync responses
    pub async fn run_message_loop_with_registry_masternode_and_blockchain(
        mut self,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        masternode_registry: Arc<crate::masternode_registry::MasternodeRegistry>,
        blockchain: Arc<Blockchain>,
    ) -> Result<(), String> {
        let mut ping_interval = interval(Self::PING_INTERVAL);
        let mut timeout_check = interval(Self::TIMEOUT_CHECK_INTERVAL);
        let mut buffer = String::new();

        info!(
            "🔄 [{:?}] Starting message loop for {} (port: {})",
            self.direction, self.peer_ip, self.remote_port
        );

        // Register this outbound connection in the peer registry so sync can reach it
        peer_registry
            .register_peer_shared(self.peer_ip.clone(), self.shared_writer())
            .await;
        info!(
            "📝 [{:?}] Registered {} in PeerConnectionRegistry for sync",
            self.direction, self.peer_ip
        );

        // Send initial handshake (required by protocol)
        let handshake = NetworkMessage::Handshake {
            magic: *b"TIME",
            protocol_version: 1,
            network: "mainnet".to_string(),
        };

        if let Err(e) = self.send_message(&handshake).await {
            error!(
                "❌ [{:?}] Failed to send handshake to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        info!(
            "🤝 [{:?}] Sent handshake to {}",
            self.direction, self.peer_ip
        );

        // Send initial ping
        if let Err(e) = self.send_ping().await {
            error!(
                "❌ [{:?}] Failed to send initial ping to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        loop {
            tokio::select! {
                // Receive messages from peer
                result = self.reader.read_line(&mut buffer) => {
                    match result {
                        Ok(0) => {
                            info!("🔌 [{:?}] Connection to {} closed by peer (EOF)",
                                  self.direction, self.peer_ip);
                            break;
                        }
                        Ok(_) => {
                            if let Err(e) = self.handle_message_with_blockchain(&buffer, &peer_registry, &masternode_registry, &blockchain).await {
                                warn!("⚠️ [{:?}] Error handling message from {}: {}",
                                      self.direction, self.peer_ip, e);
                            }
                            buffer.clear();
                        }
                        Err(e) => {
                            error!("❌ [{:?}] Error reading from {}: {}",
                                   self.direction, self.peer_ip, e);
                            break;
                        }
                    }
                }

                // Send periodic pings
                _ = ping_interval.tick() => {
                    if let Err(e) = self.send_ping().await {
                        error!("❌ [{:?}] Failed to send ping to {}: {}",
                               self.direction, self.peer_ip, e);
                        break;
                    }
                }

                // Check for timeout
                _ = timeout_check.tick() => {
                    if self.should_disconnect(&peer_registry).await {
                        error!("❌ [{:?}] Disconnecting {} due to timeout",
                               self.direction, self.peer_ip);
                        break;
                    }
                }
            }
        }

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Handle a single message with blockchain access for block sync
    async fn handle_message_with_blockchain(
        &self,
        line: &str,
        peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        masternode_registry: &Arc<crate::masternode_registry::MasternodeRegistry>,
        blockchain: &Arc<Blockchain>,
    ) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }

        // Check if this peer is whitelisted (trusted masternode from time-coin.io)
        let is_whitelisted = peer_registry.is_whitelisted(&self.peer_ip).await;

        let message: NetworkMessage =
            serde_json::from_str(line).map_err(|e| format!("Failed to parse message: {}", e))?;

        match &message {
            NetworkMessage::Ping { nonce, timestamp } => {
                self.handle_ping(*nonce, *timestamp).await?;
            }
            NetworkMessage::Pong { nonce, timestamp } => {
                self.handle_pong(*nonce, *timestamp).await?;
            }
            NetworkMessage::BlocksResponse(blocks) | NetworkMessage::BlockRangeResponse(blocks) => {
                // SIMPLIFIED FORK RESOLUTION - delegates to blockchain layer
                let block_count = blocks.len();
                if block_count == 0 {
                    debug!(
                        "📥 [{:?}] Received empty blocks response from {}",
                        self.direction, self.peer_ip
                    );
                    return Ok(());
                }

                let start_height = blocks.first().map(|b| b.header.height).unwrap_or(0);
                let end_height = blocks.last().map(|b| b.header.height).unwrap_or(0);
                let our_height = blockchain.get_height().await;

                // Update our knowledge of peer's height
                let current_known = self.peer_height.read().await;
                if current_known.is_none()
                    || (current_known.is_some() && current_known.unwrap() < end_height)
                {
                    *self.peer_height.write().await = Some(end_height);
                }
                drop(current_known);

                info!(
                    "📥 [{:?}] Received {} blocks (height {}-{}) from {} (our height: {})",
                    self.direction, block_count, start_height, end_height, self.peer_ip, our_height
                );

                // CONSENSUS CHECK: If we're significantly behind, verify this peer is on consensus chain
                let peer_tip = self.peer_height.read().await.unwrap_or(end_height);
                if peer_tip > our_height + 50 {
                    // Get all connected peers
                    let connected_peers = peer_registry.get_connected_peers().await;

                    // Count how many peers have similar height to this peer (consensus check)
                    let mut supporting_peers_count = 0;
                    let mut total_peers_with_height = 0;

                    for peer_ip in &connected_peers {
                        if let Some(height) = peer_registry.get_peer_height(peer_ip).await {
                            total_peers_with_height += 1;
                            // Peer agrees if within 10 blocks
                            if (height as i64 - peer_tip as i64).abs() <= 10 {
                                supporting_peers_count += 1;
                            }
                        }
                    }

                    // If less than 50% of peers agree with this peer, it's not canonical
                    if total_peers_with_height > 0
                        && (supporting_peers_count as f64 / total_peers_with_height as f64) < 0.5
                    {
                        info!(
                            "📊 Peer {} NOT on consensus chain ({}/{} peers agree at height {}). Deferring to periodic consensus.",
                            self.peer_ip, supporting_peers_count, total_peers_with_height, peer_tip
                        );
                        // Try to add sequential blocks, but don't do fork resolution
                        let mut added = 0;
                        for block in blocks {
                            match blockchain.add_block_with_fork_handling(block.clone()).await {
                                Ok(true) => added += 1,
                                Ok(false) | Err(_) => break,
                            }
                        }
                        if added > 0 {
                            info!("✅ Added {} sequential blocks from {}", added, self.peer_ip);
                        }
                        return Ok(());
                    }

                    info!(
                        "✅ Peer {} IS on consensus chain ({}/{} peers agree at height {}). Proceeding with fork resolution.",
                        self.peer_ip, supporting_peers_count, total_peers_with_height, peer_tip
                    );
                }

                // FIRST: Check if we have matching blocks (common ancestor search)
                let mut common_ancestor: Option<u64> = None;
                if start_height <= our_height {
                    // Find the last matching block in this batch
                    for block in blocks.iter() {
                        if block.header.height <= our_height {
                            if let Ok(our_block) = blockchain.get_block(block.header.height) {
                                if our_block.hash() == block.hash() {
                                    common_ancestor = Some(block.header.height);
                                } else if common_ancestor.is_some() {
                                    // Found mismatch after common ancestor - this is the fork point
                                    break;
                                }
                            }
                        }
                    }
                }

                // SECOND: If we found common ancestor with longer chain, try to reorganize
                if let Some(ancestor) = common_ancestor {
                    let peer_tip_height = self
                        .peer_height
                        .read()
                        .await
                        .unwrap_or(end_height)
                        .max(end_height);

                    if peer_tip_height > our_height {
                        info!(
                            "📊 Peer has longer chain ({} > {}) with common ancestor at {}",
                            peer_tip_height, our_height, ancestor
                        );

                        // Store the common ancestor in tracker to prevent re-triggering block-1 search
                        let mut tracker = self.fork_resolution_tracker.write().await;
                        if let Some(ref mut attempt) = *tracker {
                            attempt.common_ancestor = Some(ancestor);
                        } else {
                            *tracker = Some(ForkResolutionAttempt {
                                fork_height: ancestor,
                                attempt_count: 1,
                                last_attempt: std::time::Instant::now(),
                                common_ancestor: Some(ancestor),
                                peer_height: peer_tip_height,
                            });
                        }
                        drop(tracker);

                        // Collect blocks after common ancestor
                        let mut reorg_blocks: Vec<Block> = blocks
                            .iter()
                            .filter(|b| b.header.height > ancestor)
                            .cloned()
                            .collect();
                        reorg_blocks.sort_by_key(|b| b.header.height);

                        if reorg_blocks.is_empty()
                            || reorg_blocks.last().unwrap().header.height < peer_tip_height
                        {
                            // Need more blocks - request from ancestor to peer tip
                            let request_start = if reorg_blocks.is_empty() {
                                ancestor + 1
                            } else {
                                reorg_blocks.last().unwrap().header.height + 1
                            };
                            info!(
                                "📤 Requesting blocks {}-{} for complete chain",
                                request_start, peer_tip_height
                            );
                            let msg = NetworkMessage::GetBlocks(request_start, peer_tip_height + 1);
                            if let Err(e) = self.send_message(&msg).await {
                                warn!("Failed to request blocks: {}", e);
                            }
                            return Ok(());
                        }

                        // Check for gaps in blocks
                        let has_gaps = reorg_blocks
                            .windows(2)
                            .any(|w| w[1].header.height != w[0].header.height + 1);
                        if has_gaps || reorg_blocks.first().unwrap().header.height != ancestor + 1 {
                            info!(
                                "📤 Detected gaps, requesting complete chain from {}",
                                ancestor + 1
                            );
                            let msg = NetworkMessage::GetBlocks(ancestor + 1, peer_tip_height + 1);
                            if let Err(e) = self.send_message(&msg).await {
                                warn!("Failed to request blocks: {}", e);
                            }
                            return Ok(());
                        }

                        // We have complete chain - decide if we should reorganize
                        match blockchain
                            .should_accept_fork(&reorg_blocks, peer_tip_height, &self.peer_ip)
                            .await
                        {
                            Ok(true) => {
                                info!(
                                    "🔄 Reorganizing to peer chain from height {} with {} blocks",
                                    ancestor,
                                    reorg_blocks.len()
                                );
                                match blockchain.reorganize_to_chain(ancestor, reorg_blocks).await {
                                    Ok(_) => {
                                        info!("✅ Chain reorganization successful");
                                        *self.fork_resolution_tracker.write().await = None;
                                        return Ok(());
                                    }
                                    Err(e) => {
                                        error!("❌ Chain reorganization failed: {}", e);
                                    }
                                }
                            }
                            Ok(false) => {
                                info!("❌ Keeping our chain - peer chain rejected");
                                *self.fork_resolution_tracker.write().await = None;
                                return Ok(());
                            }
                            Err(e) => {
                                warn!("⚠️ Fork resolution error: {}", e);
                            }
                        }
                    }
                }

                // THIRD: Only if no common ancestor found (either in this batch OR in tracker), do block-1 search
                let tracker = self.fork_resolution_tracker.read().await;
                let already_have_ancestor =
                    tracker.as_ref().and_then(|t| t.common_ancestor).is_some();
                drop(tracker);

                if common_ancestor.is_none()
                    && !already_have_ancestor
                    && start_height > 0
                    && start_height <= our_height
                {
                    if let Ok(our_block) = blockchain.get_block(start_height) {
                        let incoming_hash = blocks[0].hash();
                        let our_hash = our_block.hash();

                        if incoming_hash != our_hash {
                            warn!(
                                "🔀 Fork detected at height {}: our {} vs peer {}",
                                start_height,
                                hex::encode(&our_hash[..8]),
                                hex::encode(&incoming_hash[..8])
                            );

                            // Track attempts to prevent infinite loops
                            let mut tracker = self.fork_resolution_tracker.write().await;
                            let search_depth = our_height.saturating_sub(start_height);

                            // Check if we should give up
                            if let Some(ref mut attempt) = *tracker {
                                if start_height < attempt.fork_height {
                                    // We're searching backwards - increment
                                    if attempt.should_give_up() {
                                        error!(
                                            "🚨 Fork resolution failed: timeout after {} seconds (searched {} blocks back)",
                                            attempt.last_attempt.elapsed().as_secs(), search_depth
                                        );
                                        *tracker = None;
                                        drop(tracker);
                                        return Err("Fork resolution failed - timeout".to_string());
                                    }
                                    attempt.increment();
                                    attempt.fork_height = start_height;
                                } else if start_height == attempt.fork_height {
                                    // Same height - duplicate response, don't increment
                                } else {
                                    // New fork
                                    *tracker =
                                        Some(ForkResolutionAttempt::new(start_height, end_height));
                                }
                            } else {
                                // First attempt
                                *tracker =
                                    Some(ForkResolutionAttempt::new(start_height, end_height));
                            }

                            // Safety check - only reject if chains are truly incompatible (>2000 blocks)
                            if search_depth > 2000 {
                                error!(
                                    "🚨 Searched back {} blocks - chains incompatible",
                                    search_depth
                                );
                                *tracker = None;
                                drop(tracker);
                                return Err(
                                    "Deep fork >2000 blocks - chains incompatible".to_string()
                                );
                            }

                            drop(tracker);

                            // SIMPLE STRATEGY: Go back one block at a time to find common ancestor
                            let check_height = start_height - 1;
                            info!(
                                "📤 Fork at height {}. Checking previous block at height {} (searched {} blocks back)",
                                start_height, check_height, search_depth
                            );
                            let msg = NetworkMessage::GetBlocks(check_height, check_height + 1);
                            if let Err(e) = self.send_message(&msg).await {
                                warn!("Failed to request block for fork resolution: {}", e);
                            }
                            return Ok(());
                        }
                    }
                }

                // FOURTH: Try to add blocks sequentially if no fork handling triggered
                let mut added = 0;
                let mut skipped = 0;
                let mut corrupt_blocks = 0;

                for block in blocks {
                    // Validate block has non-zero previous_hash (except genesis at height 0)
                    if block.header.height > 0 && block.header.previous_hash == [0u8; 32] {
                        warn!(
                            "⚠️ [{:?}] Peer {} sent corrupt block {} with zero previous_hash - skipping",
                            self.direction, self.peer_ip, block.header.height
                        );
                        corrupt_blocks += 1;
                        skipped += 1;
                        continue;
                    }

                    match blockchain.add_block_with_fork_handling(block.clone()).await {
                        Ok(true) => added += 1,
                        Ok(false) => skipped += 1,
                        Err(e) => {
                            if skipped < 3 {
                                warn!("⏭️ Skipped block {}: {}", block.header.height, e);
                            }
                            skipped += 1;
                        }
                    }
                }

                // If too many corrupt blocks, disconnect this peer
                if corrupt_blocks > 5 {
                    return Err(format!(
                        "Peer {} sent {} corrupt blocks - disconnecting",
                        self.peer_ip, corrupt_blocks
                    ));
                }

                if added > 0 {
                    info!(
                        "✅ [{:?}] Synced {} blocks from {} (skipped {})",
                        self.direction, added, self.peer_ip, skipped
                    );
                    *self.invalid_block_count.write().await = 0;
                } else if skipped > 0 {
                    warn!(
                        "⚠️ [{:?}] All {} blocks skipped from {}",
                        self.direction, skipped, self.peer_ip
                    );

                    // If this is the consensus peer and all blocks were skipped, we need fork resolution
                    if skipped == block_count && peer_tip > our_height + 50 {
                        // Check if this peer is on consensus chain
                        let connected_peers = peer_registry.get_connected_peers().await;
                        let mut supporting_peers_count = 0;
                        let mut total_peers_with_height = 0;

                        for peer_ip in &connected_peers {
                            if let Some(height) = peer_registry.get_peer_height(peer_ip).await {
                                total_peers_with_height += 1;
                                if (height as i64 - peer_tip as i64).abs() <= 10 {
                                    supporting_peers_count += 1;
                                }
                            }
                        }

                        if total_peers_with_height > 0
                            && (supporting_peers_count as f64 / total_peers_with_height as f64)
                                >= 0.5
                        {
                            warn!(
                                "🔀 All blocks skipped from consensus peer {}. Starting fork resolution from block {}.",
                                self.peer_ip, start_height
                            );
                            // Trigger block-1 search to find common ancestor
                            let check_height = start_height.saturating_sub(1);
                            let msg = NetworkMessage::GetBlocks(check_height, check_height + 1);
                            if let Err(e) = self.send_message(&msg).await {
                                warn!("Failed to request block for fork resolution: {}", e);
                            }
                        }
                    }
                }
            }
            NetworkMessage::BlockInventory(block_height) => {
                // Handle block inventory announcement - only request if we need it
                let our_height = blockchain.get_height().await;

                if *block_height > our_height {
                    // Check if the gap is too large (more than 1 block)
                    let gap = block_height - our_height;

                    if gap > 1 {
                        // We're far behind - don't request this specific block
                        // Instead, let the sync mechanism handle catchup
                        debug!(
                            "📊 [{:?}] Peer {} announced block {} but we're at {} (gap: {}). Ignoring - will sync via GetBlocks.",
                            self.direction, self.peer_ip, block_height, our_height, gap
                        );
                        // No action - the sync loop will handle catching up
                    } else {
                        // Gap is 1 - this is the next block we need
                        debug!(
                            "📥 [{:?}] Requesting block {} from {} (next block after our {})",
                            self.direction, block_height, self.peer_ip, our_height
                        );
                        let request = NetworkMessage::BlockRequest(*block_height);
                        if let Err(e) = self.send_message(&request).await {
                            warn!(
                                "⚠️ [{:?}] Failed to request block {} from {}: {}",
                                self.direction, block_height, self.peer_ip, e
                            );
                        }
                    }
                } else {
                    // We already have this block or are ahead, ignore silently
                    // This is normal and expected - reduces log spam
                }
            }
            NetworkMessage::BlockRequest(block_height) => {
                // Peer is requesting a specific block from us
                info!(
                    "📨 [{:?}] Received block request for height {} from {}",
                    self.direction, block_height, self.peer_ip
                );

                if let Ok(block) = blockchain.get_block_by_height(*block_height).await {
                    let response = NetworkMessage::BlockResponse(block);
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send block {} to {}: {}",
                            self.direction, block_height, self.peer_ip, e
                        );
                    } else {
                        info!(
                            "✅ [{:?}] Sent block {} to {}",
                            self.direction, block_height, self.peer_ip
                        );
                    }
                } else {
                    debug!(
                        "⚠️ [{:?}] Don't have block {} requested by {}",
                        self.direction, block_height, self.peer_ip
                    );
                }
            }
            NetworkMessage::BlockResponse(block) => {
                // Handle block response to our request
                let block_height = block.header.height;
                let our_height = blockchain.get_height().await;

                info!(
                    "📦 [{:?}] Received block {} from {} (our height: {})",
                    self.direction, block_height, self.peer_ip, our_height
                );

                match blockchain.add_block_with_fork_handling(block.clone()).await {
                    Ok(true) => {
                        info!(
                            "✅ [{:?}] Added block {} from {}",
                            self.direction, block_height, self.peer_ip
                        );
                    }
                    Ok(false) => {
                        warn!(
                            "⏭️ [{:?}] Skipped block {} from {} (already have or invalid)",
                            self.direction, block_height, self.peer_ip
                        );
                    }
                    Err(e) => {
                        warn!(
                            "⏭️ [{:?}] Skipped block {} from {}: {}",
                            self.direction, block_height, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::BlockAnnouncement(block) => {
                // Keep legacy full block announcement support for backward compatibility
                let block_height = block.header.height;
                let our_height = blockchain.get_height().await;

                debug!(
                    "📦 [{:?}] Received block announcement {} from {} (our height: {})",
                    self.direction, block_height, self.peer_ip, our_height
                );

                match blockchain.add_block_with_fork_handling(block.clone()).await {
                    Ok(true) => {
                        info!(
                            "✅ [{:?}] Added announced block {} from {}",
                            self.direction, block_height, self.peer_ip
                        );
                        // Reset invalid counter on successful block
                        *self.invalid_block_count.write().await = 0;
                    }
                    Ok(false) | Err(_) => {
                        // Block was skipped or had error - likely a fork
                        let mut count = self.invalid_block_count.write().await;
                        *count += 1;

                        if is_whitelisted {
                            // AGGRESSIVE FORK RESOLUTION for whitelisted peers
                            // These are trusted masternodes - we need to sync with them
                            warn!(
                                "🔀 [{:?}] Whitelisted peer {} block {} rejected (count: {}) - triggering fork resolution",
                                self.direction, self.peer_ip, block_height, *count
                            );

                            // Request their chain to compare and potentially reorg
                            // Start from a few blocks before where we diverge
                            let request_from = our_height.saturating_sub(5);
                            let request_to = block_height.max(our_height) + 10;

                            info!(
                                "🔄 [{:?}] Requesting blocks {}-{} from whitelisted peer {} for fork resolution",
                                self.direction, request_from, request_to, self.peer_ip
                            );

                            let msg = NetworkMessage::GetBlocks(request_from, request_to);
                            if let Err(e) = self.send_message(&msg).await {
                                warn!("Failed to request fork resolution blocks: {}", e);
                            }

                            // Reset counter after triggering resolution
                            if *count >= 5 {
                                *count = 0;
                            }
                        } else {
                            // Non-whitelisted peers get disconnected after repeated failures
                            warn!(
                                "⏭️ [{:?}] Skipped block {} from non-whitelisted {} (count: {})",
                                self.direction, block_height, self.peer_ip, *count
                            );

                            if *count >= 5 {
                                error!(
                                    "🚫 [{:?}] Non-whitelisted peer {} sent {} invalid blocks - disconnecting",
                                    self.direction, self.peer_ip, *count
                                );
                                return Err(format!(
                                    "Peer {} sent {} invalid blocks",
                                    self.peer_ip, *count
                                ));
                            }
                        }
                    }
                }
            }
            NetworkMessage::GenesisAnnouncement(block) => {
                // Special handling for genesis block announcements
                if block.header.height != 0 {
                    warn!(
                        "⚠️ [{:?}] Received GenesisAnnouncement for non-genesis block {} from {}",
                        self.direction, block.header.height, self.peer_ip
                    );
                    return Ok(());
                }

                // Check if we already have genesis - try to get block at height 0
                if blockchain.get_block_by_height(0).await.is_ok() {
                    debug!(
                        "⏭️ [{:?}] Ignoring genesis announcement from {} (already have genesis)",
                        self.direction, self.peer_ip
                    );
                    return Ok(());
                }

                info!(
                    "📦 [{:?}] Received genesis announcement from {}",
                    self.direction, self.peer_ip
                );

                // Simply verify basic genesis structure
                use crate::block::genesis::GenesisBlock;
                match GenesisBlock::verify_structure(block) {
                    Ok(()) => {
                        info!("✅ Genesis structure validation passed, adding to chain");

                        match blockchain.add_block(block.clone()).await {
                            Ok(()) => {
                                info!(
                                    "✅ [{:?}] Genesis block added successfully from {}, hash: {}",
                                    self.direction,
                                    self.peer_ip,
                                    hex::encode(&block.hash()[..8])
                                );
                                // Reset invalid counter
                                *self.invalid_block_count.write().await = 0;
                            }
                            Err(e) => {
                                error!(
                                    "❌ [{:?}] Failed to add genesis block from {}: {}",
                                    self.direction, self.peer_ip, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "⚠️ [{:?}] Genesis validation failed from {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::RequestGenesis => {
                info!(
                    "📥 [{:?}] Received genesis request from {}",
                    self.direction, self.peer_ip
                );

                // If we have genesis, send it to the requester
                match blockchain.get_block_by_height(0).await {
                    Ok(genesis) => {
                        info!(
                            "📤 [{:?}] Sending genesis block to {}",
                            self.direction, self.peer_ip
                        );
                        let msg = NetworkMessage::GenesisAnnouncement(genesis);
                        if let Err(e) = self.send_message(&msg).await {
                            warn!(
                                "⚠️ [{:?}] Failed to send genesis to {}: {}",
                                self.direction, self.peer_ip, e
                            );
                        }
                    }
                    Err(_) => {
                        debug!(
                            "⚠️ [{:?}] Cannot fulfill genesis request from {} - we don't have genesis yet",
                            self.direction, self.peer_ip
                        );
                    }
                }
            }
            NetworkMessage::BlockHeightResponse(peer_height) => {
                // Store peer's height for fork resolution (both locally and in registry)
                *self.peer_height.write().await = Some(*peer_height);

                // Also store in peer registry for fork detection
                peer_registry
                    .set_peer_height(&self.peer_ip, *peer_height)
                    .await;

                let our_height = blockchain.get_height().await;

                // If peer has higher height, first determine canonical chain before requesting
                if *peer_height > our_height {
                    info!(
                        "📈 [{:?}] Peer {} reported higher height {} (we have {})",
                        self.direction, self.peer_ip, peer_height, our_height
                    );

                    // CONSENSUS CHECK: Determine canonical peer before requesting blocks
                    if let Some((consensus_height, consensus_peer)) =
                        blockchain.compare_chain_with_peers().await
                    {
                        if consensus_height > our_height {
                            if self.peer_ip == consensus_peer {
                                info!(
                                    "✅ Peer {} IS the canonical peer. Requesting blocks {}-{}",
                                    self.peer_ip, our_height, peer_height
                                );
                                let msg = NetworkMessage::GetBlocks(our_height, *peer_height + 1);
                                if let Err(e) = self.send_message(&msg).await {
                                    warn!("Failed to request verification blocks: {}", e);
                                }
                            } else {
                                info!(
                                    "📊 Peer {} is not canonical peer (consensus: {}). Deferring to canonical sync.",
                                    self.peer_ip, consensus_peer
                                );
                                // Don't request blocks from non-canonical peers
                            }
                        }
                    } else {
                        // Fallback: No consensus found, request from this peer anyway
                        debug!(
                            "No consensus found, requesting blocks from {}",
                            self.peer_ip
                        );
                        let msg = NetworkMessage::GetBlocks(our_height, *peer_height + 1);
                        if let Err(e) = self.send_message(&msg).await {
                            warn!("Failed to request verification blocks: {}", e);
                        }
                    }
                } else if *peer_height == our_height {
                    // Same height - should verify we're on the same chain
                    // Request the tip block to compare hashes
                    debug!(
                        "🔍 [{:?}] Peer {} at same height {}, requesting tip for verification",
                        self.direction, self.peer_ip, peer_height
                    );
                    let msg = NetworkMessage::GetBlocks(*peer_height, *peer_height + 1);
                    if let Err(e) = self.send_message(&msg).await {
                        debug!("Failed to request tip verification: {}", e);
                    }
                } else {
                    debug!(
                        "📊 [{:?}] Peer {} at lower height {} (we have {})",
                        self.direction, self.peer_ip, peer_height, our_height
                    );
                }
            }
            NetworkMessage::ChainTipResponse { height, hash } => {
                // Compare peer's chain tip with ours for fork detection
                let our_height = blockchain.get_height().await;
                let our_hash = blockchain.get_block_hash(our_height).unwrap_or([0u8; 32]);

                // Store peer height
                *self.peer_height.write().await = Some(*height);
                peer_registry.set_peer_height(&self.peer_ip, *height).await;

                if *height == our_height {
                    // Same height - check if same hash (on same chain)
                    if *hash != our_hash {
                        // FORK DETECTED - same height but different blocks!
                        warn!(
                            "🔀 [{:?}] FORK with {} at height {}: our {} vs their {}",
                            self.direction,
                            self.peer_ip,
                            height,
                            hex::encode(&our_hash[..8]),
                            hex::encode(&hash[..8])
                        );

                        // Request blocks to determine which chain to follow
                        let request_from = height.saturating_sub(10);
                        info!(
                            "🔄 [{:?}] Requesting blocks {}-{} from {} for fork resolution",
                            self.direction,
                            request_from,
                            height + 5,
                            self.peer_ip
                        );
                        let msg = NetworkMessage::GetBlocks(request_from, *height + 5);
                        if let Err(e) = self.send_message(&msg).await {
                            warn!("Failed to request fork resolution blocks: {}", e);
                        }
                    } else {
                        debug!(
                            "✅ [{:?}] Peer {} on same chain at height {}",
                            self.direction, self.peer_ip, height
                        );
                    }
                } else if *height > our_height {
                    // Peer is ahead - we might need to sync
                    info!(
                        "📈 [{:?}] Peer {} ahead at height {} (we have {}), requesting blocks",
                        self.direction, self.peer_ip, height, our_height
                    );
                    let msg = NetworkMessage::GetBlocks(our_height + 1, *height + 1);
                    if let Err(e) = self.send_message(&msg).await {
                        warn!("Failed to request sync blocks: {}", e);
                    }
                } else {
                    // We're ahead - peer might need to sync from us
                    debug!(
                        "📉 [{:?}] Peer {} behind at height {} (we have {})",
                        self.direction, self.peer_ip, height, our_height
                    );
                }
            }
            NetworkMessage::MasternodeAnnouncement {
                address,
                reward_address,
                tier,
                public_key,
            } => {
                // Register masternode from announcement
                let masternode = crate::types::Masternode {
                    address: address.clone(),
                    wallet_address: reward_address.clone(),
                    tier: *tier,
                    public_key: *public_key,
                    collateral: 0,
                    registered_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                };
                if let Err(e) = masternode_registry
                    .register(masternode, reward_address.clone())
                    .await
                {
                    warn!(
                        "⚠️ [{:?}] Failed to register masternode {} from announcement: {:?}",
                        self.direction, address, e
                    );
                } else {
                    info!(
                        "✅ [{:?}] Registered masternode {} from announcement",
                        self.direction, address
                    );
                }
            }
            NetworkMessage::MasternodesResponse(masternodes) => {
                // Register all masternodes from response
                info!(
                    "📥 [{:?}] Processing MasternodesResponse from {} with {} masternode(s)",
                    self.direction,
                    self.peer_ip,
                    masternodes.len()
                );

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let mut registered = 0;
                for mn_data in masternodes {
                    let masternode = crate::types::Masternode {
                        address: mn_data.address.clone(),
                        wallet_address: mn_data.reward_address.clone(),
                        tier: mn_data.tier,
                        public_key: mn_data.public_key,
                        collateral: 0,
                        registered_at: now,
                    };

                    if masternode_registry
                        .register_internal(masternode, mn_data.reward_address.clone(), false)
                        .await
                        .is_ok()
                    {
                        registered += 1;
                    }
                }

                if registered > 0 {
                    info!(
                        "✅ [{:?}] Registered {} masternode(s) from response",
                        self.direction, registered
                    );
                }
            }
            NetworkMessage::GetMasternodes => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext {
                    blockchain: Arc::clone(blockchain),
                    peer_registry: Arc::clone(peer_registry),
                    masternode_registry: Arc::clone(masternode_registry),
                    consensus: None, // Not needed for GetMasternodes
                    block_cache: None,
                    broadcast_tx: None,
                };

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send response to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::GetBlocks(_start, _end) => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext {
                    blockchain: Arc::clone(blockchain),
                    peer_registry: Arc::clone(peer_registry),
                    masternode_registry: Arc::clone(masternode_registry),
                    consensus: None, // Not needed for GetBlocks
                    block_cache: None,
                    broadcast_tx: None,
                };

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send response to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::TSCDBlockProposal { .. }
            | NetworkMessage::TSCDPrepareVote { .. }
            | NetworkMessage::TSCDPrecommitVote { .. } => {
                // Use unified message handler for TSDC messages with shared resources
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);

                // Get TSDC resources from peer registry
                let (consensus, block_cache, broadcast_tx) =
                    peer_registry.get_tsdc_resources().await;

                let context = MessageContext {
                    blockchain: Arc::clone(blockchain),
                    peer_registry: Arc::clone(peer_registry),
                    masternode_registry: Arc::clone(masternode_registry),
                    consensus,
                    block_cache,
                    broadcast_tx,
                };

                if let Err(e) = handler.handle_message(&message, &context).await {
                    warn!(
                        "⚠️ [{:?}] Error handling TSDC message from {}: {}",
                        self.direction, self.peer_ip, e
                    );
                }
            }
            _ => {
                debug!(
                    "📨 [{:?}] Received other message from {}",
                    self.direction, self.peer_ip
                );
            }
        }

        Ok(())
    }

    /// Handle a single message with masternode registry for registration
    async fn handle_message_with_masternode_registry(
        &self,
        line: &str,
        _peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        masternode_registry: &Arc<crate::masternode_registry::MasternodeRegistry>,
    ) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }

        let message: NetworkMessage =
            serde_json::from_str(line).map_err(|e| format!("Failed to parse message: {}", e))?;

        match &message {
            NetworkMessage::Ping { nonce, timestamp } => {
                self.handle_ping(*nonce, *timestamp).await?;
            }
            NetworkMessage::Pong { nonce, timestamp } => {
                self.handle_pong(*nonce, *timestamp).await?;
            }
            NetworkMessage::MasternodeAnnouncement {
                address,
                reward_address,
                tier,
                public_key,
            } => {
                // Register masternode from announcement
                let masternode = crate::types::Masternode {
                    address: address.clone(),
                    wallet_address: reward_address.clone(),
                    tier: *tier,
                    public_key: *public_key,
                    collateral: 0,
                    registered_at: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                };
                if let Err(e) = masternode_registry
                    .register(masternode, reward_address.clone())
                    .await
                {
                    warn!(
                        "⚠️ [{:?}] Failed to register masternode {} from announcement: {:?}",
                        self.direction, address, e
                    );
                } else {
                    info!(
                        "✅ [{:?}] Registered masternode {} from announcement",
                        self.direction, address
                    );
                }
            }
            NetworkMessage::MasternodesResponse(masternodes) => {
                // Register all masternodes from response
                info!(
                    "📥 [{:?}] Processing MasternodesResponse from {} with {} masternode(s)",
                    self.direction,
                    self.peer_ip,
                    masternodes.len()
                );

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let mut registered = 0;
                for mn_data in masternodes {
                    let masternode = crate::types::Masternode {
                        address: mn_data.address.clone(),
                        wallet_address: mn_data.reward_address.clone(),
                        tier: mn_data.tier,
                        public_key: mn_data.public_key,
                        collateral: 0,
                        registered_at: now,
                    };

                    if masternode_registry
                        .register_internal(masternode, mn_data.reward_address.clone(), false)
                        .await
                        .is_ok()
                    {
                        registered += 1;
                    }
                }

                if registered > 0 {
                    info!(
                        "✅ [{:?}] Registered {} masternode(s) from response",
                        self.direction, registered
                    );
                }
            }
            NetworkMessage::GetMasternodes => {
                debug!(
                    "📥 [{:?}] Received GetMasternodes request from {}",
                    self.direction, self.peer_ip
                );
            }
            _ => {
                debug!(
                    "📨 [{:?}] Received message from {} (type: {})",
                    self.direction,
                    self.peer_ip,
                    match &message {
                        NetworkMessage::TransactionBroadcast(_) => "TransactionBroadcast",
                        NetworkMessage::BlockAnnouncement(_) => "BlockAnnouncement",
                        NetworkMessage::BlockInventory(_) => "BlockInventory",
                        NetworkMessage::BlockRequest(_) => "BlockRequest",
                        NetworkMessage::BlockResponse(_) => "BlockResponse",
                        NetworkMessage::Handshake { .. } => "Handshake",
                        _ => "Other",
                    }
                );
            }
        }

        Ok(())
    }

    /// Handle a single message with peer registry for master node discovery
    async fn handle_message_with_registry(
        &self,
        line: &str,
        _peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }

        let message: NetworkMessage =
            serde_json::from_str(line).map_err(|e| format!("Failed to parse message: {}", e))?;

        match &message {
            NetworkMessage::Ping { nonce, timestamp } => {
                self.handle_ping(*nonce, *timestamp).await?;
            }
            NetworkMessage::Pong { nonce, timestamp } => {
                self.handle_pong(*nonce, *timestamp).await?;
            }
            NetworkMessage::MasternodeAnnouncement {
                address,
                reward_address: _,
                tier: _,
                public_key: _,
            } => {
                // Log received masternode announcement from outbound connection
                info!(
                    "📨 [{:?}] Received masternode announcement from {} for IP: {}",
                    self.direction, self.peer_ip, address
                );
                // NOTE: Full processing happens in NetworkServer for inbound connections
                // For outbound connections, we just log - NetworkServer handles the registration
            }
            NetworkMessage::GetMasternodes => {
                // Outbound connection received GetMasternodes request
                debug!(
                    "📥 [{:?}] Received GetMasternodes request from {}",
                    self.direction, self.peer_ip
                );
            }
            NetworkMessage::MasternodesResponse(masternodes) => {
                // Outbound connection received masternode list from peer
                debug!(
                    "📥 [{:?}] Received MasternodesResponse from {} with {} masternode(s)",
                    self.direction,
                    self.peer_ip,
                    masternodes.len()
                );
            }
            _ => {
                // Other message types are logged but not processed here
                debug!(
                    "📨 [{:?}] Received message from {} (type: {})",
                    self.direction,
                    self.peer_ip,
                    match &message {
                        NetworkMessage::TransactionBroadcast(_) => "TransactionBroadcast",
                        NetworkMessage::BlockAnnouncement(_) => "BlockAnnouncement",
                        NetworkMessage::BlockInventory(_) => "BlockInventory",
                        NetworkMessage::BlockRequest(_) => "BlockRequest",
                        NetworkMessage::BlockResponse(_) => "BlockResponse",
                        NetworkMessage::Handshake { .. } => "Handshake",
                        _ => "Other",
                    }
                );
            }
        }

        Ok(())
    }

    /// Run the unified message loop for this connection
    #[allow(dead_code)]
    pub async fn run_message_loop(mut self) -> Result<(), String> {
        let mut ping_interval = interval(Self::PING_INTERVAL);
        let mut timeout_check = interval(Self::TIMEOUT_CHECK_INTERVAL);
        let mut buffer = String::new();

        info!(
            "🔄 [{:?}] Starting message loop for {} (port: {})",
            self.direction, self.peer_ip, self.remote_port
        );

        // Send initial handshake (required by protocol)
        let handshake = NetworkMessage::Handshake {
            magic: *b"TIME",
            protocol_version: 1,
            network: "mainnet".to_string(),
        };

        if let Err(e) = self.send_message(&handshake).await {
            error!(
                "❌ [{:?}] Failed to send handshake to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        info!(
            "🤝 [{:?}] Sent handshake to {}",
            self.direction, self.peer_ip
        );

        // Send initial ping
        if let Err(e) = self.send_ping().await {
            error!(
                "❌ [{:?}] Failed to send initial ping to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        loop {
            tokio::select! {
                // Receive messages from peer
                result = self.reader.read_line(&mut buffer) => {
                    match result {
                        Ok(0) => {
                            info!("🔌 [{:?}] Connection to {} closed by peer (EOF)",
                                  self.direction, self.peer_ip);
                            break;
                        }
                        Ok(_) => {
                            if let Err(e) = self.handle_message(&buffer).await {
                                warn!("⚠️ [{:?}] Error handling message from {}: {}",
                                      self.direction, self.peer_ip, e);
                            }
                            buffer.clear();
                        }
                        Err(e) => {
                            error!("❌ [{:?}] Error reading from {}: {}",
                                   self.direction, self.peer_ip, e);
                            break;
                        }
                    }
                }

                // Send periodic pings
                _ = ping_interval.tick() => {
                    if let Err(e) = self.send_ping().await {
                        error!("❌ [{:?}] Failed to send ping to {}: {}",
                               self.direction, self.peer_ip, e);
                        break;
                    }
                }

                // Check for timeout (simplified for dead code method)
                _ = timeout_check.tick() => {
                    let mut state = self.ping_state.write().await;
                    if state.check_timeout(Self::MAX_MISSED_PONGS, Self::PONG_TIMEOUT) {
                        error!("❌ [{:?}] Disconnecting {} due to timeout",
                               self.direction, self.peer_ip);
                        break;
                    }
                }
            }
        }

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Handle a single message
    #[allow(dead_code)]
    async fn handle_message(&self, line: &str) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }

        let message: NetworkMessage =
            serde_json::from_str(line).map_err(|e| format!("Failed to parse message: {}", e))?;

        match &message {
            NetworkMessage::Ping { nonce, timestamp } => {
                self.handle_ping(*nonce, *timestamp).await?;
            }
            NetworkMessage::Pong { nonce, timestamp } => {
                self.handle_pong(*nonce, *timestamp).await?;
            }
            _ => {
                // Other message types are handled by peer_registry or other handlers
                // Just log that we received them (don't silently drop)
                debug!(
                    "📨 [{:?}] Received message from {} (type: {})",
                    self.direction,
                    self.peer_ip,
                    match &message {
                        NetworkMessage::TransactionBroadcast(_) => "TransactionBroadcast",
                        NetworkMessage::BlockAnnouncement(_) => "BlockAnnouncement",
                        NetworkMessage::BlockInventory(_) => "BlockInventory",
                        NetworkMessage::BlockRequest(_) => "BlockRequest",
                        NetworkMessage::BlockResponse(_) => "BlockResponse",
                        NetworkMessage::MasternodeAnnouncement { .. } => "MasternodeAnnouncement",
                        NetworkMessage::Handshake { .. } => "Handshake",
                        _ => "Other",
                    }
                );
                // Message will be handled by peer_registry broadcast or other channels
            }
        }

        Ok(())
    }
}

// ===== PeerStateManager (formerly in peer_state.rs) =====

use std::collections::HashMap;
use tokio::sync::mpsc;

/// Manages all active peer connections
#[allow(dead_code)]
pub struct PeerStateManager {
    /// Active connections by IP address (only one connection per IP)
    connections: Arc<RwLock<HashMap<String, PeerConnectionState>>>,
}

/// Active connection state for a peer
#[derive(Clone)]
#[allow(dead_code)]
pub struct PeerConnectionState {
    /// Peer's IP address (unique identifier)
    pub ip: String,

    /// Channel to send messages to this peer
    pub tx: mpsc::UnboundedSender<NetworkMessage>,

    /// When this connection was established
    pub connected_at: std::time::Instant,

    /// Last successful ping/pong time
    pub last_activity: Arc<RwLock<std::time::Instant>>,

    /// Missed ping count
    pub missed_pings: Arc<RwLock<u32>>,
}

impl PeerConnectionState {
    fn new(ip: String, tx: mpsc::UnboundedSender<NetworkMessage>) -> Self {
        let now = std::time::Instant::now();
        Self {
            ip,
            tx,
            connected_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            missed_pings: Arc::new(RwLock::new(0)),
        }
    }

    async fn mark_active(&self) {
        let mut last = self.last_activity.write().await;
        *last = std::time::Instant::now();

        let mut missed = self.missed_pings.write().await;
        *missed = 0;
    }

    async fn increment_missed_pings(&self) -> u32 {
        let mut missed = self.missed_pings.write().await;
        *missed += 1;
        *missed
    }

    async fn idle_duration(&self) -> Duration {
        let last = self.last_activity.read().await;
        std::time::Instant::now().duration_since(*last)
    }

    fn send(&self, message: NetworkMessage) -> Result<(), String> {
        self.tx
            .send(message)
            .map_err(|e| format!("Failed to send message: {}", e))
    }
}

#[allow(dead_code)]
impl PeerStateManager {
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_connection(
        &self,
        ip: String,
        tx: mpsc::UnboundedSender<NetworkMessage>,
    ) -> Result<bool, String> {
        let mut conns = self.connections.write().await;

        if conns.contains_key(&ip) {
            return Ok(false);
        }

        let conn = PeerConnectionState::new(ip.clone(), tx);
        conns.insert(ip, conn);
        Ok(true)
    }

    pub async fn remove_connection(&self, ip: &str) -> Option<PeerConnectionState> {
        let mut conns = self.connections.write().await;
        conns.remove(ip)
    }

    pub async fn get_connection(&self, ip: &str) -> Option<PeerConnectionState> {
        let conns = self.connections.read().await;
        conns.get(ip).cloned()
    }

    pub async fn has_connection(&self, ip: &str) -> bool {
        let conns = self.connections.read().await;
        conns.contains_key(ip)
    }

    pub async fn connection_count(&self) -> usize {
        let conns = self.connections.read().await;
        conns.len()
    }

    pub async fn get_all_ips(&self) -> Vec<String> {
        let conns = self.connections.read().await;
        conns.keys().cloned().collect()
    }

    pub async fn get_all_connections(&self) -> Vec<PeerConnectionState> {
        let conns = self.connections.read().await;
        conns.values().cloned().collect()
    }

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

    pub async fn send_to_peer(&self, ip: &str, message: NetworkMessage) -> Result<(), String> {
        let conns = self.connections.read().await;
        if let Some(conn) = conns.get(ip) {
            conn.send(message)
        } else {
            Err(format!("No connection to peer {}", ip))
        }
    }

    pub async fn mark_peer_active(&self, ip: &str) {
        let conns = self.connections.read().await;
        if let Some(conn) = conns.get(ip) {
            conn.mark_active().await;
        }
    }

    pub async fn increment_missed_pings(&self, ip: &str) -> Option<u32> {
        let conns = self.connections.read().await;
        if let Some(conn) = conns.get(ip) {
            Some(conn.increment_missed_pings().await)
        } else {
            None
        }
    }

    pub async fn get_idle_connections(&self, idle_threshold: Duration) -> Vec<PeerConnectionState> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ping_state_new() {
        let state = PingState::new();
        assert_eq!(state.missed_pongs, 0);
        assert!(state.pending_pings.is_empty());
        assert!(state.last_ping_sent.is_none());
        assert!(state.last_pong_received.is_none());
    }

    #[test]
    fn test_record_ping_sent() {
        let mut state = PingState::new();
        state.record_ping_sent(12345);

        assert_eq!(state.pending_pings.len(), 1);
        assert_eq!(state.pending_pings[0].0, 12345);
        assert!(state.last_ping_sent.is_some());
    }

    #[test]
    fn test_record_pong_matching() {
        let mut state = PingState::new();
        state.record_ping_sent(12345);

        let matched = state.record_pong_received(12345);

        assert!(matched);
        assert!(state.pending_pings.is_empty());
        assert_eq!(state.missed_pongs, 0);
    }

    #[test]
    fn test_record_pong_non_matching() {
        let mut state = PingState::new();
        state.record_ping_sent(12345);

        let matched = state.record_pong_received(99999);

        assert!(!matched);
        assert_eq!(state.pending_pings.len(), 1); // Original ping still there
    }

    #[test]
    fn test_multiple_pending_pings() {
        let mut state = PingState::new();

        // Send multiple pings
        for i in 1..=5 {
            state.record_ping_sent(i);
        }

        assert_eq!(state.pending_pings.len(), 5);

        // Respond to one
        let matched = state.record_pong_received(3);
        assert!(matched);
        assert_eq!(state.pending_pings.len(), 4);
    }

    #[test]
    fn test_pending_pings_limit() {
        let mut state = PingState::new();

        // Send 7 pings (more than the limit of 5)
        for i in 1..=7 {
            state.record_ping_sent(i);
        }

        // Should only keep last 5
        assert_eq!(state.pending_pings.len(), 5);

        // Oldest ping (1 and 2) should be removed
        let nonces: Vec<u64> = state.pending_pings.iter().map(|(n, _)| *n).collect();
        assert_eq!(nonces, vec![3, 4, 5, 6, 7]);
    }

    #[test]
    fn test_direction_inbound() {
        let dir = ConnectionDirection::Inbound;
        assert_eq!(dir, ConnectionDirection::Inbound);
        assert_ne!(dir, ConnectionDirection::Outbound);
    }

    #[test]
    fn test_direction_outbound() {
        let dir = ConnectionDirection::Outbound;
        assert_eq!(dir, ConnectionDirection::Outbound);
        assert_ne!(dir, ConnectionDirection::Inbound);
    }

    #[test]
    fn test_ping_state_reset_on_pong() {
        let mut state = PingState::new();
        state.missed_pongs = 5; // Simulate some missed pongs

        state.record_ping_sent(100);
        let matched = state.record_pong_received(100);

        assert!(matched);
        assert_eq!(state.missed_pongs, 0); // Should be reset
    }
}
