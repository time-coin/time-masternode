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

use crate::blockchain::Blockchain;
use crate::network::message::NetworkMessage;
use crate::network::message_handler::{ConnectionDirection, MessageContext, MessageHandler};

/// State for tracking ping/pong health
#[derive(Debug)]
pub struct PingState {
    last_ping_sent: Option<Instant>,
    last_pong_received: Option<Instant>,
    pending_pings: Vec<(u64, Instant)>, // (nonce, sent_time)
    missed_pongs: u32,
    pub last_rtt_ms: Option<f64>, // Latest one-way latency in milliseconds (RTT/2)
}

// Circuit breaker limits for fork resolution
const FORK_RESOLUTION_TIMEOUT_SECS: u64 = 60; // 60 seconds - fast fork resolution
const CRITICAL_FORK_DEPTH: u64 = 100; // Log warning if fork > 100 blocks (but don't stop)
const PROGRESS_LOG_INTERVAL: u32 = 10; // Log progress every 10 attempts

/// Fork resolution attempt tracker with enhanced circuit breaker
#[derive(Debug, Clone)]
struct ForkResolutionAttempt {
    fork_height: u64,
    attempt_count: u32,
    last_attempt: std::time::Instant,
    common_ancestor: Option<u64>,
    peer_height: u64,
    max_depth_searched: u64, // Track deepest search to prevent infinite loops
}

impl ForkResolutionAttempt {
    fn new(fork_height: u64, peer_height: u64) -> Self {
        Self {
            fork_height,
            attempt_count: 1,
            last_attempt: std::time::Instant::now(),
            common_ancestor: None,
            peer_height,
            max_depth_searched: 0,
        }
    }

    fn is_same_fork(&self, fork_height: u64, peer_height: u64) -> bool {
        // Consider it the same fork if heights are within 10 blocks
        (self.fork_height as i64 - fork_height as i64).abs() <= 10
            && (self.peer_height as i64 - peer_height as i64).abs() <= 10
    }

    fn should_give_up(&self) -> bool {
        let elapsed = self.last_attempt.elapsed();

        // Only give up on timeout - no attempt or depth limits
        if elapsed.as_secs() > FORK_RESOLUTION_TIMEOUT_SECS {
            tracing::error!(
                "🚨 Fork resolution timeout: {} seconds exceeded (attempt {}, depth {})",
                FORK_RESOLUTION_TIMEOUT_SECS,
                self.attempt_count,
                self.max_depth_searched
            );
            return true;
        }

        // Log progress periodically to show we're still working
        if self.attempt_count % PROGRESS_LOG_INTERVAL == 0 {
            tracing::warn!(
                "🔄 Fork resolution in progress: attempt {}, searched {} blocks back, fork at height {}",
                self.attempt_count,
                self.max_depth_searched,
                self.fork_height
            );
        }

        // Warn if fork is very deep but keep going
        if self.max_depth_searched > CRITICAL_FORK_DEPTH && self.max_depth_searched % 100 == 0 {
            tracing::warn!(
                "⚠️ Deep fork: searched {} blocks back - this may take a while",
                self.max_depth_searched
            );
        }

        false
    }

    fn update_depth(&mut self, current_height: u64, search_height: u64) {
        let depth = current_height.saturating_sub(search_height);
        if depth > self.max_depth_searched {
            self.max_depth_searched = depth;

            // Log warning for deep forks
            if depth > CRITICAL_FORK_DEPTH {
                tracing::warn!(
                    "⚠️  Deep fork detected: {} blocks back (critical threshold: {})",
                    depth,
                    CRITICAL_FORK_DEPTH
                );
            }
        }
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
            last_rtt_ms: None,
        }
    }

    fn record_ping_sent(&mut self, nonce: u64) {
        let now = Instant::now();
        self.last_ping_sent = Some(now);

        // Hard limit to prevent memory exhaustion on high packet-loss networks
        const MAX_PENDING_PINGS: usize = 100;
        if self.pending_pings.len() >= MAX_PENDING_PINGS {
            // Remove oldest 50% when limit reached
            self.pending_pings.drain(0..50);
            tracing::warn!(
                "Pending pings exceeded {}, cleared old entries",
                MAX_PENDING_PINGS
            );
        }

        self.pending_pings.push((nonce, now));

        // Remove pings that have already timed out (older than 120 seconds)
        // Increased timeout for high-latency satellite/mobile connections
        const TIMEOUT: Duration = Duration::from_secs(120);
        self.pending_pings
            .retain(|(_, sent_time)| now.duration_since(*sent_time) <= TIMEOUT);
    }

    fn record_pong_received(&mut self, nonce: u64) -> bool {
        let now = Instant::now();
        self.last_pong_received = Some(now);

        // Find and remove the matching ping, calculating one-way latency
        if let Some(pos) = self.pending_pings.iter().position(|(n, _)| *n == nonce) {
            let (_nonce, sent_time) = self.pending_pings.remove(pos);

            // Calculate round-trip time and divide by 2 for one-way latency
            let rtt = now.duration_since(sent_time);
            self.last_rtt_ms = Some(rtt.as_secs_f64() * 1000.0 / 2.0);

            self.missed_pongs = 0; // Reset counter on successful pong
            true
        } else {
            debug!(
                "🔀 Received pong for unknown nonce: {} (likely duplicate connection)",
                nonce
            );
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

    /// Last opportunistic sync request time (for throttling)
    last_opportunistic_sync: Arc<RwLock<Option<Instant>>>,

    /// Whitelist status - whitelisted masternodes get relaxed ping/pong timeouts
    is_whitelisted: bool,
}

/// Configuration for peer connection message loop
/// Supports optional components for different connection scenarios
pub struct MessageLoopConfig {
    /// Required: Peer registry for tracking active connections
    pub peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,

    /// Optional: Masternode registry for masternode-specific handling
    pub masternode_registry: Option<Arc<crate::masternode_registry::MasternodeRegistry>>,

    /// Optional: Blockchain for block synchronization and validation
    pub blockchain: Option<Arc<Blockchain>>,

    /// Optional: Broadcast receiver for forwarding gossip and other broadcasts
    pub broadcast_rx:
        Option<tokio::sync::broadcast::Receiver<crate::network::message::NetworkMessage>>,
}

impl MessageLoopConfig {
    /// Create a new config with just peer registry (minimal setup)
    pub fn new(
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) -> Self {
        Self {
            peer_registry,
            masternode_registry: None,
            blockchain: None,
            broadcast_rx: None,
        }
    }

    /// Add masternode registry (builder pattern)
    pub fn with_masternode_registry(
        mut self,
        registry: Arc<crate::masternode_registry::MasternodeRegistry>,
    ) -> Self {
        self.masternode_registry = Some(registry);
        self
    }

    /// Add blockchain (builder pattern)
    pub fn with_blockchain(mut self, blockchain: Arc<Blockchain>) -> Self {
        self.blockchain = Some(blockchain);
        self
    }

    /// Add broadcast receiver (builder pattern)
    pub fn with_broadcast_rx(
        mut self,
        rx: tokio::sync::broadcast::Receiver<crate::network::message::NetworkMessage>,
    ) -> Self {
        self.broadcast_rx = Some(rx);
        self
    }
}

impl PeerConnection {
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    const TIMEOUT_CHECK_INTERVAL: Duration = Duration::from_secs(10);
    const PONG_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes - very generous
    const MAX_MISSED_PONGS: u32 = 10; // Allow many missed pongs before disconnect

    // Phase 1: Relaxed timeouts for whitelisted masternodes
    const WHITELISTED_PONG_TIMEOUT: Duration = Duration::from_secs(600); // 10 minutes
    const WHITELISTED_MAX_MISSED_PONGS: u32 = 20; // Allow even more missed pongs

    /// Create a new outbound connection to a peer
    pub async fn new_outbound(
        peer_ip: String,
        port: u16,
        is_whitelisted: bool,
    ) -> Result<Self, String> {
        let addr = format!("{}:{}", peer_ip, port);

        if is_whitelisted {
            info!("🔗 [OUTBOUND-WHITELIST] Connecting to masternode {}", addr);
        } else {
            info!("🔗 [OUTBOUND] Connecting to {}", addr);
        }

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
            last_opportunistic_sync: Arc::new(RwLock::new(None)),
            local_port: local_addr.port(),
            remote_port: remote_addr.port(),
            is_whitelisted,
        })
    }

    /// Create a new inbound connection from a peer
    #[allow(dead_code)]
    pub async fn new_inbound(stream: TcpStream, is_whitelisted: bool) -> Result<Self, String> {
        let peer_addr = stream
            .peer_addr()
            .map_err(|e| format!("Failed to get peer address: {}", e))?;

        let local_addr = stream
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?;

        let peer_ip = peer_addr.ip().to_string();

        if is_whitelisted {
            info!(
                "🔗 [INBOUND-WHITELIST] Accepted masternode connection from {}",
                peer_addr
            );
        } else {
            info!("🔗 [Inbound] Accepted connection from {}", peer_addr);
        }

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
            last_opportunistic_sync: Arc::new(RwLock::new(None)),
            local_port: local_addr.port(),
            remote_port: peer_addr.port(),
            is_whitelisted,
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

    /// Get the latest ping RTT in seconds (for RPC)
    pub async fn get_ping_rtt(&self) -> Option<f64> {
        let state = self.ping_state.read().await;
        state.last_rtt_ms.map(|ms| ms / 1000.0) // Convert ms to seconds
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
    /// Phase 3: Now includes blockchain height in ping
    async fn send_ping(&self, blockchain: Option<&Arc<Blockchain>>) -> Result<(), String> {
        let nonce = rand::random::<u64>();
        let timestamp = chrono::Utc::now().timestamp();
        // Phase 3: Get our height if blockchain available
        let height = blockchain.map(|bc| bc.get_height());

        {
            let mut state = self.ping_state.write().await;
            state.record_ping_sent(nonce);
        }

        // Phase 3: Include height in log if available
        let height_info = height
            .map(|h| format!(" at height {}", h))
            .unwrap_or_default();
        info!(
            "📤 [{:?}] Sent ping to {}{} (nonce: {})",
            self.direction, self.peer_ip, height_info, nonce
        );

        self.send_message(&NetworkMessage::Ping {
            nonce,
            timestamp,
            height,
        })
        .await
    }

    /// Handle received ping
    async fn handle_ping(
        &self,
        nonce: u64,
        _timestamp: i64,
        peer_height: Option<u64>,
        our_height: Option<u64>,
    ) -> Result<(), String> {
        // Phase 3: Log peer height from ping
        let height_info = peer_height
            .map(|h| format!(" at height {}", h))
            .unwrap_or_default();
        info!(
            "📨 [{:?}] Received ping from {}{}  (nonce: {})",
            self.direction, self.peer_ip, height_info, nonce
        );

        // Phase 3: Update peer height if provided
        if let Some(height) = peer_height {
            *self.peer_height.write().await = Some(height);
        }

        let timestamp = chrono::Utc::now().timestamp();
        // Phase 3: Include our height in pong response
        self.send_message(&NetworkMessage::Pong {
            nonce,
            timestamp,
            height: our_height,
        })
        .await?;

        info!(
            "✅ [{:?}] Sent pong to {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        Ok(())
    }

    /// Handle received pong
    async fn handle_pong(
        &self,
        nonce: u64,
        _timestamp: i64,
        peer_height: Option<u64>,
    ) -> Result<(), String> {
        // Phase 3: Log peer height from pong
        let height_info = peer_height
            .map(|h| format!(" at height {}", h))
            .unwrap_or_default();
        info!(
            "📨 [{:?}] Received pong from {}{}  (nonce: {})",
            self.direction, self.peer_ip, height_info, nonce
        );

        // Phase 3: Update peer height if provided
        if let Some(height) = peer_height {
            *self.peer_height.write().await = Some(height);
        }

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
            // Check if this could be from a duplicate connection
            // If we have no pending pings at all, this is likely from another connection instance
            if state.pending_pings.is_empty() {
                debug!(
                    "🔀 [{:?}] Received pong from {} (nonce: {}) but no pending pings - likely duplicate connection or peer bug",
                    self.direction,
                    self.peer_ip,
                    nonce
                );
            } else {
                // If we have pending pings but wrong nonce, could be cross-connection mixing
                // This happens when both inbound and outbound connections exist to same peer
                debug!(
                    "🔀 [{:?}] Pong nonce mismatch from {} (got: {}, expected one of: {:?}) - possibly duplicate connection",
                    self.direction,
                    self.peer_ip,
                    nonce,
                    state
                        .pending_pings
                        .iter()
                        .map(|(n, _)| n)
                        .collect::<Vec<_>>()
                );
            }
            Ok(())
        }
    }

    /// Check if other peers agree with fork from this peer (lightweight consensus)
    /// Returns the number of peers that agree (are within 10 blocks of fork height)
    async fn check_fork_agreement(
        &self,
        peer_registry: &Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
        fork_height: u64,
        exclude_peer: &str,
    ) -> Result<usize, String> {
        let connected_peers = peer_registry.get_connected_peers().await;

        if connected_peers.len() < 2 {
            // Not enough peers to check consensus
            return Ok(0);
        }

        let mut agreement_count = 0;

        for peer_ip in &connected_peers {
            if peer_ip == exclude_peer {
                continue; // Skip the peer we're checking
            }

            if let Some(peer_height) = peer_registry.get_peer_height(peer_ip).await {
                // Peer agrees if within 10 blocks of fork height
                if (peer_height as i64 - fork_height as i64).abs() <= 10 {
                    agreement_count += 1;
                    debug!(
                        "Peer {} agrees with fork (height {} vs fork {})",
                        peer_ip, peer_height, fork_height
                    );
                }
            }
        }

        Ok(agreement_count)
    }

    /// Check if connection should be closed due to timeout
    async fn should_disconnect(
        &self,
        _peer_registry: &crate::network::peer_connection_registry::PeerConnectionRegistry,
    ) -> bool {
        // CRITICAL FIX: Whitelisted masternodes should NEVER be disconnected due to timeout
        // They are essential network infrastructure and must maintain persistent connections
        if self.is_whitelisted {
            let state = self.ping_state.read().await;
            // Log warning for monitoring but DO NOT disconnect
            if state.missed_pongs > Self::WHITELISTED_MAX_MISSED_PONGS {
                warn!(
                    "⚠️ [{:?}] Whitelisted masternode {} has {} missed pongs (NOT disconnecting - protected)",
                    self.direction, self.peer_ip, state.missed_pongs
                );
            }
            return false; // ✅ FIX: Never disconnect whitelisted nodes
        }

        // Non-whitelisted peers: Use normal timeout logic
        let mut state = self.ping_state.write().await;
        if state.check_timeout(Self::MAX_MISSED_PONGS, Self::PONG_TIMEOUT) {
            warn!(
                "❌ [{:?}] Disconnecting non-whitelisted peer {} after {} missed pongs",
                self.direction, self.peer_ip, state.missed_pongs
            );
            true
        } else {
            false
        }
    }

    /// Run the unified message loop for this connection with broadcast channel integration
    ///
    /// **DEPRECATED**: Use `run_message_loop_unified()` with `MessageLoopConfig` instead.
    /// This provides better flexibility with the builder pattern.
    ///
    /// # Migration
    /// ```
    /// // Old way:
    /// peer_connection.run_message_loop_with_registry(peer_registry).await?;
    ///
    /// // New way:
    /// let config = MessageLoopConfig::new(peer_registry);
    /// peer_connection.run_message_loop_unified(config).await?;
    /// ```
    #[deprecated(
        since = "1.0.0",
        note = "Use run_message_loop_unified() with MessageLoopConfig for better flexibility"
    )]
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
        // Phase 3: No blockchain available in this loop - pass None
        if let Err(e) = self.send_ping(None).await {
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
                    // Phase 3: No blockchain available in this loop - pass None
                    if let Err(e) = self.send_ping(None).await {
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

        // Clear stale peer data on disconnect to prevent reporting old heights
        peer_registry.clear_peer_data(&self.peer_ip).await;

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Run the unified message loop with masternode registry integration
    ///
    /// **DEPRECATED**: Use `run_message_loop_unified()` with `MessageLoopConfig` instead.
    #[deprecated(
        since = "1.0.0",
        note = "Use run_message_loop_unified() with MessageLoopConfig for better flexibility"
    )]
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
        // Phase 3: No blockchain in this loop - pass None
        if let Err(e) = self.send_ping(None).await {
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
                    // Phase 3: No blockchain in this loop - pass None
                    if let Err(e) = self.send_ping(None).await {
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

        // Clear stale peer data on disconnect to prevent reporting old heights
        peer_registry.clear_peer_data(&self.peer_ip).await;

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Run the unified message loop with masternode registry AND blockchain integration
    /// This is used for outbound connections that need to receive block sync responses
    ///
    /// **DEPRECATED**: Use `run_message_loop_unified()` with `MessageLoopConfig` instead.
    #[deprecated(
        since = "1.0.0",
        note = "Use run_message_loop_unified() with MessageLoopConfig for better flexibility"
    )]
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
        // Phase 3: Pass blockchain for height information
        if let Err(e) = self.send_ping(Some(&blockchain)).await {
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
                    // Phase 3: Pass blockchain for height information
                    if let Err(e) = self.send_ping(Some(&blockchain)).await {
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

        // Clear stale peer data on disconnect to prevent reporting old heights
        peer_registry.clear_peer_data(&self.peer_ip).await;

        info!(
            "🔌 [{:?}] Message loop ended for {}",
            self.direction, self.peer_ip
        );
        Ok(())
    }

    /// Unified message handler that delegates to MessageHandler
    /// This replaces the old handle_message_with_* functions
    async fn handle_message_unified(
        &self,
        line: &str,
        config: &MessageLoopConfig,
    ) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }

        // Parse message
        let message: NetworkMessage =
            serde_json::from_str(line).map_err(|e| format!("Failed to parse message: {}", e))?;

        // Handle connection-level messages that need special state management
        match &message {
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => {
                let our_height = config.blockchain.as_ref().map(|bc| bc.get_height());
                return self
                    .handle_ping(*nonce, *timestamp, *height, our_height)
                    .await;
            }
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => {
                return self.handle_pong(*nonce, *timestamp, *height).await;
            }
            NetworkMessage::Handshake { .. }
            | NetworkMessage::Ack { .. }
            | NetworkMessage::Version { .. } => {
                // Connection-level messages - not handled by MessageHandler
                debug!(
                    "📨 [{:?}] Received connection-level message from {}",
                    self.direction, self.peer_ip
                );
                return Ok(());
            }
            _ => {
                // All other messages go through MessageHandler
            }
        }

        // Build context for MessageHandler
        let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);

        // Create context with available components
        let context = if let Some(ref blockchain) = config.blockchain {
            let masternode_registry = config
                .masternode_registry
                .as_ref()
                .expect("Masternode registry required when blockchain is provided");
            MessageContext::minimal(
                Arc::clone(blockchain),
                Arc::clone(&config.peer_registry),
                Arc::clone(masternode_registry),
            )
        } else if let Some(ref _masternode_registry) = config.masternode_registry {
            // This case should not happen in practice - we always have blockchain when we have masternode_registry
            return Err("Cannot create context without blockchain".to_string());
        } else {
            return Err("Cannot create context without masternode registry".to_string());
        };

        // Delegate to MessageHandler
        match handler.handle_message(&message, &context).await {
            Ok(Some(response)) => {
                // Send response if MessageHandler returned one
                if let Err(e) = self.send_message(&response).await {
                    warn!(
                        "⚠️ [{:?}] Failed to send response to {}: {}",
                        self.direction, self.peer_ip, e
                    );
                }
                Ok(())
            }
            Ok(None) => {
                // Message handled successfully, no response needed
                Ok(())
            }
            Err(e) => {
                debug!(
                    "⚠️ [{:?}] MessageHandler error for {} (may be normal): {}",
                    self.direction, self.peer_ip, e
                );
                Ok(()) // Don't propagate handler errors as connection errors
            }
        }
    }

    /// Handle a single message with blockchain access for block sync
    /// DEPRECATED: Use handle_message_unified instead
    #[allow(dead_code)]
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
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: Pass peer height and our height to handler
                let our_height = Some(blockchain.get_height());
                self.handle_ping(*nonce, *timestamp, *height, our_height)
                    .await?;
            }
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: Pass peer height to handler
                self.handle_pong(*nonce, *timestamp, *height).await?;
            }
            NetworkMessage::BlocksResponse(_) | NetworkMessage::BlockRangeResponse(_) => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send response after BlocksResponse handling: {}",
                            self.direction, e
                        );
                    }
                }
            }
            NetworkMessage::BlockInventory(block_height) => {
                // Handle block inventory announcement - request immediately to stay synced
                let our_height = blockchain.get_height();

                if *block_height > our_height {
                    // Check if the gap is large (more than 1 block)
                    let gap = block_height - our_height;

                    if gap > 1 {
                        // We're behind - request range immediately for fast sync
                        debug!(
                            "📊 [{:?}] Peer {} announced block {} but we're at {} (gap: {}). Requesting range immediately.",
                            self.direction, self.peer_ip, block_height, our_height, gap
                        );
                        // Request the full range to catch up immediately
                        let start = our_height + 1;
                        let end = *block_height;
                        let request = NetworkMessage::GetBlocks(start, end);
                        if let Err(e) = self.send_message(&request).await {
                            warn!(
                                "⚠️ [{:?}] Failed to request blocks {}-{} from {}: {}",
                                self.direction, start, end, self.peer_ip, e
                            );
                        } else {
                            debug!(
                                "📤 [{:?}] Requested blocks {}-{} from {} for immediate sync",
                                self.direction, start, end, self.peer_ip
                            );
                        }
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
                let our_height = blockchain.get_height();

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
                let our_height = blockchain.get_height();

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

                let our_height = blockchain.get_height();

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
                let our_height = blockchain.get_height();
                let our_hash = blockchain.get_block_hash(our_height).unwrap_or([0u8; 32]);

                // Store peer height and chain tip
                *self.peer_height.write().await = Some(*height);
                peer_registry.set_peer_height(&self.peer_ip, *height).await;
                peer_registry
                    .update_peer_chain_tip(&self.peer_ip, *height, *hash)
                    .await;

                tracing::debug!(
                    "📥 Received ChainTipResponse from {}: height {} hash {} (our height: {})",
                    self.peer_ip,
                    height,
                    hex::encode(&hash[..8]),
                    our_height
                );

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

                        // Check consensus - if we have majority, alert the peer
                        let all_peers = peer_registry.get_connected_peers().await;
                        let mut our_chain_count = 1; // Count ourselves
                        let mut peer_chain_count = 0;

                        for peer_addr in &all_peers {
                            if let Some((peer_h, peer_hash)) =
                                peer_registry.get_peer_chain_tip(peer_addr).await
                            {
                                if peer_h == our_height {
                                    if peer_hash == our_hash {
                                        our_chain_count += 1;
                                    } else if peer_hash == *hash {
                                        peer_chain_count += 1;
                                    }
                                }
                            }
                        }

                        // If we have consensus and peer is on minority fork, alert them
                        if our_chain_count > peer_chain_count && our_chain_count >= 3 {
                            info!(
                                "📢 [{:?}] Alerting {} they're on minority fork ({} peers on our chain, {} on theirs)",
                                self.direction, self.peer_ip, our_chain_count, peer_chain_count
                            );

                            let alert = NetworkMessage::ForkAlert {
                                your_height: *height,
                                your_hash: *hash,
                                consensus_height: our_height,
                                consensus_hash: our_hash,
                                consensus_peer_count: our_chain_count,
                                message: format!(
                                    "You're on a minority fork at height {}. {} peers (including us) are on consensus chain with hash {}",
                                    height,
                                    our_chain_count,
                                    hex::encode(&our_hash[..8])
                                ),
                            };

                            if let Err(e) = self.send_message(&alert).await {
                                warn!("Failed to send fork alert: {}", e);
                            }
                        }

                        // Fork resolution must ALWAYS proceed - consensus depends on it
                        // The periodic compare_chain_with_peers() will handle resolution
                        info!(
                            "🔀 [{:?}] Fork detected with {} at height {} - will be resolved by periodic consensus check",
                            self.direction,
                            self.peer_ip,
                            height
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
                    debug!(
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
            NetworkMessage::MasternodeAnnouncement { .. } => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                let _ = handler.handle_message(&message, &context).await;
            }
            NetworkMessage::MasternodesResponse(_) => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                let _ = handler.handle_message(&message, &context).await;
            }
            NetworkMessage::GetChainTip => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send ChainTipResponse to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::GetMasternodes => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

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
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send response to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::TransactionBroadcast(_) => {
                // Use unified message handler with consensus context
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let (consensus, block_cache, broadcast_tx) =
                    peer_registry.get_tsdc_resources().await;

                let mut context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );
                context.consensus = consensus;
                context.block_cache = block_cache;
                context.broadcast_tx = broadcast_tx;

                if let Err(e) = handler.handle_message(&message, &context).await {
                    warn!(
                        "⚠️ [{:?}] Error handling transaction from {}: {}",
                        self.direction, self.peer_ip, e
                    );
                }
            }
            NetworkMessage::GetBlockHeight => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send BlockHeightResponse to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::GetPeers => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send PeersResponse to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::PeersResponse(_) => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                // MessageHandler handles adding to discovered_peers
                let _ = handler.handle_message(&message, &context).await;
            }
            // Heartbeat messages removed - using TCP connection state instead
            NetworkMessage::GetBlockRange { .. } => {
                // Use unified message handler
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Ok(Some(response)) = handler.handle_message(&message, &context).await {
                    if let Err(e) = self.send_message(&response).await {
                        warn!(
                            "⚠️ [{:?}] Failed to send BlockRangeResponse to {}: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            NetworkMessage::TimeLockBlockProposal { .. }
            | NetworkMessage::TimeVotePrepare { .. }
            | NetworkMessage::TimeVotePrecommit { .. } => {
                // Use unified message handler for TSDC messages with shared resources
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);

                // Get TSDC resources from peer registry
                let (consensus, block_cache, broadcast_tx) =
                    peer_registry.get_tsdc_resources().await;

                let mut context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );
                context.consensus = consensus;
                context.block_cache = block_cache;
                context.broadcast_tx = broadcast_tx;

                if let Err(e) = handler.handle_message(&message, &context).await {
                    warn!(
                        "⚠️ [{:?}] Error handling TSDC message from {}: {}",
                        self.direction, self.peer_ip, e
                    );
                }
            }
            NetworkMessage::MasternodeStatusGossip { .. } => {
                // Use unified message handler for gossip
                let handler = MessageHandler::new(self.peer_ip.clone(), self.direction);
                let context = MessageContext::minimal(
                    Arc::clone(blockchain),
                    Arc::clone(peer_registry),
                    Arc::clone(masternode_registry),
                );

                if let Err(e) = handler.handle_message(&message, &context).await {
                    warn!(
                        "⚠️ [{:?}] Error handling gossip from {}: {}",
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
    /// DEPRECATED: Use handle_message_unified instead
    #[allow(dead_code)]
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
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: No blockchain in this handler
                self.handle_ping(*nonce, *timestamp, *height, None).await?;
            }
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: Pass peer height
                self.handle_pong(*nonce, *timestamp, *height).await?;
            }
            NetworkMessage::ForkAlert {
                your_height,
                your_hash,
                consensus_height,
                consensus_hash,
                consensus_peer_count,
                message: alert_message,
            } => {
                warn!(
                    "🚨 [{:?}] FORK ALERT from {}: {}",
                    self.direction, self.peer_ip, alert_message
                );
                warn!(
                    "   Our height {} hash {} vs Consensus height {} hash {} ({} peers)",
                    your_height,
                    hex::encode(&your_hash[..8]),
                    consensus_height,
                    hex::encode(&consensus_hash[..8]),
                    consensus_peer_count
                );

                // If we're on the minority fork, immediately request consensus chain
                if your_height == consensus_height && your_hash != consensus_hash {
                    warn!("   ⚠️ We appear to be on minority fork! Requesting consensus chain...");
                    let request_from = consensus_height.saturating_sub(10);
                    let msg = NetworkMessage::GetBlocks(request_from, *consensus_height + 5);
                    if let Err(e) = self.send_message(&msg).await {
                        warn!("Failed to request consensus chain: {}", e);
                    }

                    // Also trigger blockchain's fork resolution check
                    info!("   🔄 Triggering immediate fork resolution check");
                    // The blockchain will handle this when it receives the blocks
                }
            }
            NetworkMessage::MasternodeAnnouncement {
                address,
                reward_address,
                tier,
                public_key,
            } => {
                // Register masternode from announcement
                let masternode = crate::types::Masternode::new_legacy(
                    address.clone(),
                    reward_address.clone(),
                    0,
                    *public_key,
                    *tier,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
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
                    let masternode = crate::types::Masternode::new_legacy(
                        mn_data.address.clone(),
                        mn_data.reward_address.clone(),
                        0,
                        mn_data.public_key,
                        mn_data.tier,
                        now,
                    );

                    if masternode_registry
                        .register_internal(masternode, mn_data.reward_address.clone(), false) // Don't activate via gossip - only on connection
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
            NetworkMessage::GetChainTip => {
                // GetChainTip needs blockchain access - handled by full message loop only
                debug!(
                    "📥 [{:?}] Received GetChainTip from {} (no blockchain in this handler)",
                    self.direction, self.peer_ip
                );
            }
            NetworkMessage::MasternodeStatusGossip { .. } => {
                // Gossip messages should be handled by the main message loop, not here
                // This handler is for simple responses only
                debug!(
                    "📥 [{:?}] Gossip message from {} (should be handled by main loop)",
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
    /// DEPRECATED: Use handle_message_unified instead
    #[allow(dead_code)]
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
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: No blockchain in this loop, so can't provide our height
                self.handle_ping(*nonce, *timestamp, *height, None).await?;
            }
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: Pass peer height to handler
                self.handle_pong(*nonce, *timestamp, *height).await?;
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
            NetworkMessage::GetChainTip => {
                // GetChainTip needs blockchain access - handled by full message loop only
                debug!(
                    "📥 [{:?}] Received GetChainTip from {} (no blockchain in this handler)",
                    self.direction, self.peer_ip
                );
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
    ///
    /// **DEPRECATED**: Use `run_message_loop_unified()` with `MessageLoopConfig` instead.
    #[deprecated(
        since = "1.0.0",
        note = "Use run_message_loop_unified() with MessageLoopConfig for better flexibility"
    )]
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
        // Phase 3: No blockchain available in this loop - pass None
        if let Err(e) = self.send_ping(None).await {
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
                    // Phase 3: No blockchain available in this loop - pass None
                    if let Err(e) = self.send_ping(None).await {
                        error!("❌ [{:?}] Failed to send ping to {}: {}",
                               self.direction, self.peer_ip, e);
                        break;
                    }
                }

                // Check for timeout (simplified for dead code method)
                _ = timeout_check.tick() => {
                    let mut state = self.ping_state.write().await;

                    // CRITICAL: Whitelisted masternodes must NEVER be disconnected
                    // They are essential network infrastructure requiring persistent connections
                    if self.is_whitelisted {
                        // Log warning for monitoring but DO NOT disconnect
                        if state.missed_pongs > Self::WHITELISTED_MAX_MISSED_PONGS {
                            warn!(
                                "⚠️ [{:?}] Whitelisted masternode {} has {} missed pongs (NOT disconnecting - protected)",
                                self.direction, self.peer_ip, state.missed_pongs
                            );
                        }
                        // Continue loop - never disconnect whitelisted peers
                        continue;
                    }

                    // Non-whitelisted peers: Use normal timeout logic
                    if state.check_timeout(Self::MAX_MISSED_PONGS, Self::PONG_TIMEOUT) {
                        error!("❌ [{:?}] Disconnecting non-whitelisted peer {} due to timeout ({} missed pongs)",
                               self.direction, self.peer_ip, state.missed_pongs);
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
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: No blockchain in this loop
                self.handle_ping(*nonce, *timestamp, *height, None).await?;
            }
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => {
                // Phase 3: Pass peer height
                self.handle_pong(*nonce, *timestamp, *height).await?;
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

    /// Unified message loop that works with any combination of components
    ///
    /// This replaces the 4 separate run_message_loop variants with a single
    /// flexible implementation using the builder pattern.
    ///
    /// # Example
    /// ```
    /// // Basic setup (peer registry only)
    /// let config = MessageLoopConfig::new(peer_registry);
    /// peer_connection.run_message_loop_unified(config).await?;
    ///
    /// // With masternode registry
    /// let config = MessageLoopConfig::new(peer_registry)
    ///     .with_masternode_registry(masternode_registry);
    /// peer_connection.run_message_loop_unified(config).await?;
    ///
    /// // Full setup
    /// let config = MessageLoopConfig::new(peer_registry)
    ///     .with_masternode_registry(masternode_registry)
    ///     .with_blockchain(blockchain);
    /// peer_connection.run_message_loop_unified(config).await?;
    /// ```
    pub async fn run_message_loop_unified(
        mut self,
        mut config: MessageLoopConfig,
    ) -> Result<(), String> {
        let mut ping_interval = interval(Self::PING_INTERVAL);
        let mut timeout_check = interval(Self::TIMEOUT_CHECK_INTERVAL);
        let mut buffer = String::new();

        info!(
            "🔄 [{:?}] Starting unified message loop for {} (port: {})",
            self.direction, self.peer_ip, self.remote_port
        );

        // Register this connection in the peer registry
        config
            .peer_registry
            .register_peer_shared(self.peer_ip.clone(), self.shared_writer())
            .await;
        info!(
            "📝 [{:?}] Registered {} in PeerConnectionRegistry",
            self.direction, self.peer_ip
        );

        // Send initial handshake
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
        if let Err(e) = self.send_ping(config.blockchain.as_ref()).await {
            error!(
                "❌ [{:?}] Failed to send initial ping to {}: {}",
                self.direction, self.peer_ip, e
            );
            return Err(e);
        }

        // Extract broadcast_rx before the loop to avoid borrow checker issues
        let mut broadcast_rx = config.broadcast_rx.take();

        // Main message loop
        loop {
            tokio::select! {
                // Read incoming messages
                result = self.reader.read_line(&mut buffer) => {
                    match result {
                        Ok(0) => {
                            info!("🔌 [{:?}] Connection closed by {}", self.direction, self.peer_ip);
                            break;
                        }
                        Ok(_) => {
                            // Use unified message handler
                            let handle_result = self.handle_message_unified(&buffer, &config).await;

                            if let Err(e) = handle_result {
                                warn!("⚠️ [{:?}] Error handling message from {}: {}",
                                      self.direction, self.peer_ip, e);
                            }
                            buffer.clear();
                        }
                        Err(e) => {
                            error!("❌ [{:?}] Error reading from {}: {}", self.direction, self.peer_ip, e);
                            break;
                        }
                    }
                }

                // Forward broadcast messages to peer
                result = async {
                    if let Some(ref mut rx) = broadcast_rx {
                        rx.recv().await
                    } else {
                        // If no broadcast receiver, never resolve
                        std::future::pending().await
                    }
                } => {
                    if let Ok(msg) = result {
                        // Forward broadcast to this peer
                        if let Err(e) = self.send_message(&msg).await {
                            warn!("⚠️ [{:?}] Failed to forward broadcast to {}: {}",
                                  self.direction, self.peer_ip, e);
                        }
                    }
                }

                // Send periodic pings
                _ = ping_interval.tick() => {
                    if let Err(e) = self.send_ping(config.blockchain.as_ref()).await {
                        error!("❌ [{:?}] Failed to send ping to {}: {}", self.direction, self.peer_ip, e);
                        break;
                    }
                }

                // Check for timeout
                _ = timeout_check.tick() => {
                    if self.should_disconnect(&config.peer_registry).await {
                        error!("❌ [{:?}] Disconnecting {} due to timeout", self.direction, self.peer_ip);
                        break;
                    }
                }
            }
        }

        // Clear stale peer data on disconnect to prevent reporting old heights
        config.peer_registry.clear_peer_data(&self.peer_ip).await;

        info!(
            "🔌 [{:?}] Unified message loop ended for {}",
            self.direction, self.peer_ip
        );
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

    /// Ping state for RTT tracking
    pub ping_state: Arc<RwLock<PingState>>,
}

impl PeerConnectionState {
    fn new(
        ip: String,
        tx: mpsc::UnboundedSender<NetworkMessage>,
        ping_state: Arc<RwLock<PingState>>,
    ) -> Self {
        let now = std::time::Instant::now();
        Self {
            ip,
            tx,
            connected_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            missed_pings: Arc::new(RwLock::new(0)),
            ping_state,
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

    /// Get the latest ping RTT in seconds
    pub async fn get_ping_rtt(&self) -> Option<f64> {
        let state = self.ping_state.read().await;
        state.last_rtt_ms.map(|ms| ms / 1000.0) // Convert ms to seconds
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
        ping_state: Arc<RwLock<PingState>>,
    ) -> Result<bool, String> {
        let mut conns = self.connections.write().await;

        if conns.contains_key(&ip) {
            return Ok(false);
        }

        let conn = PeerConnectionState::new(ip.clone(), tx, ping_state);
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
    fn test_pending_pings_timeout_cleanup() {
        let mut state = PingState::new();

        // Send 7 pings immediately - all should be kept since none have timed out
        for i in 1..=7 {
            state.record_ping_sent(i);
        }

        // All pings should be kept since they're recent (within 90 second timeout)
        assert_eq!(state.pending_pings.len(), 7);

        let nonces: Vec<u64> = state.pending_pings.iter().map(|(n, _)| *n).collect();
        assert_eq!(nonces, vec![1, 2, 3, 4, 5, 6, 7]);
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
