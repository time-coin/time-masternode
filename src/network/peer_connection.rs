//! Peer Connection Management
//! Handles individual peer connections and message routing.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::{interval, Instant};
use tracing::{debug, error, info, warn};

use crate::blockchain::Blockchain;
use crate::network::blacklist::IPBlacklist;
use crate::network::message::NetworkMessage;
use crate::network::message_handler::{ConnectionDirection, MessageContext, MessageHandler};
use crate::network::tls::TlsConfig;

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

    /// Channel receiver for incoming parsed messages (from I/O bridge task)
    msg_reader: tokio::sync::mpsc::UnboundedReceiver<Result<Option<NetworkMessage>, String>>,

    /// Channel sender for outgoing serialized frame bytes (to I/O bridge task)
    writer_tx: crate::network::peer_connection_registry::PeerWriterTx,

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

    /// Optional: Blacklist for rejecting messages from banned peers
    pub blacklist: Option<Arc<RwLock<IPBlacklist>>>,

    /// Optional: AI System for recording events and making intelligent decisions
    pub ai_system: Option<Arc<crate::ai::AISystem>>,
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
            blacklist: None,
            ai_system: None,
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

    /// Add blacklist (builder pattern)
    pub fn with_blacklist(mut self, blacklist: Arc<RwLock<IPBlacklist>>) -> Self {
        self.blacklist = Some(blacklist);
        self
    }

    /// Add AI system (builder pattern)
    pub fn with_ai_system(mut self, ai_system: Arc<crate::ai::AISystem>) -> Self {
        self.ai_system = Some(ai_system);
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
        tls_config: Option<Arc<TlsConfig>>,
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

        // Create channel-based I/O to avoid tokio::io::split() on TLS streams
        let (msg_read_tx, msg_read_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<Option<NetworkMessage>, String>>();
        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

        if let Some(tls) = tls_config {
            info!("🔒 [OUTBOUND] TLS handshake with {}", addr);
            let tls_stream = tls
                .connect_client(stream, "timecoin.local")
                .await
                .map_err(|e| format!("TLS handshake failed with {}: {}", addr, e))?;
            let peer_addr = addr.clone();
            // Single I/O bridge task owns the entire TLS stream
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let mut stream = tls_stream;
                loop {
                    tokio::select! {
                        result = crate::network::wire::read_message(&mut stream) => {
                            let is_eof = matches!(&result, Ok(None));
                            let is_err = result.is_err();
                            if msg_read_tx.send(result).is_err() {
                                break;
                            }
                            if is_eof || is_err {
                                break;
                            }
                        }
                        bytes = write_rx.recv() => {
                            match bytes {
                                Some(data) => {
                                    if let Err(e) = stream.write_all(&data).await {
                                        tracing::debug!("🔒 TLS write error for {}: {}", peer_addr, e);
                                        break;
                                    }
                                    if let Err(e) = stream.flush().await {
                                        tracing::debug!("🔒 TLS flush error for {}: {}", peer_addr, e);
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                }
                tracing::debug!("🔒 TLS I/O bridge exiting for {}", peer_addr);
            });
        } else {
            let (r, w) = stream.into_split();
            let peer_addr = addr.clone();
            // Spawn reader task
            tokio::spawn(async move {
                let mut reader = r;
                loop {
                    let result = crate::network::wire::read_message(&mut reader).await;
                    let is_eof = matches!(&result, Ok(None));
                    let is_err = result.is_err();
                    if msg_read_tx.send(result).is_err() {
                        break;
                    }
                    if is_eof || is_err {
                        break;
                    }
                }
                tracing::debug!("📖 Reader task exiting for {}", peer_addr);
            });
            // Spawn writer task
            let peer_addr2 = addr.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let mut writer = w;
                while let Some(data) = write_rx.recv().await {
                    if let Err(e) = writer.write_all(&data).await {
                        tracing::debug!("📝 Write error for {}: {}", peer_addr2, e);
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        tracing::debug!("📝 Flush error for {}: {}", peer_addr2, e);
                        break;
                    }
                }
                tracing::debug!("📝 Writer task exiting for {}", peer_addr2);
            });
        }

        Ok(Self {
            peer_ip,
            direction: ConnectionDirection::Outbound,
            msg_reader: msg_read_rx,
            writer_tx: write_tx,
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
    pub async fn new_inbound(
        stream: TcpStream,
        is_whitelisted: bool,
        tls_config: Option<Arc<TlsConfig>>,
    ) -> Result<Self, String> {
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

        // Create channel-based I/O to avoid tokio::io::split() on TLS streams
        let (msg_read_tx, msg_read_rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<Option<NetworkMessage>, String>>();
        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

        if let Some(tls) = tls_config {
            info!("🔒 [INBOUND] TLS handshake with {}", peer_addr);
            let tls_stream = tls
                .accept_server(stream)
                .await
                .map_err(|e| format!("TLS accept failed from {}: {}", peer_addr, e))?;
            let addr_str = peer_addr.to_string();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let mut stream = tls_stream;
                loop {
                    tokio::select! {
                        result = crate::network::wire::read_message(&mut stream) => {
                            let is_eof = matches!(&result, Ok(None));
                            let is_err = result.is_err();
                            if msg_read_tx.send(result).is_err() {
                                break;
                            }
                            if is_eof || is_err {
                                break;
                            }
                        }
                        bytes = write_rx.recv() => {
                            match bytes {
                                Some(data) => {
                                    if let Err(e) = stream.write_all(&data).await {
                                        tracing::debug!("🔒 TLS write error for {}: {}", addr_str, e);
                                        break;
                                    }
                                    if let Err(e) = stream.flush().await {
                                        tracing::debug!("🔒 TLS flush error for {}: {}", addr_str, e);
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                }
                tracing::debug!("🔒 TLS I/O bridge exiting for {}", addr_str);
            });
        } else {
            let (r, w) = stream.into_split();
            let addr_str = peer_addr.to_string();
            tokio::spawn(async move {
                let mut reader = r;
                loop {
                    let result = crate::network::wire::read_message(&mut reader).await;
                    let is_eof = matches!(&result, Ok(None));
                    let is_err = result.is_err();
                    if msg_read_tx.send(result).is_err() {
                        break;
                    }
                    if is_eof || is_err {
                        break;
                    }
                }
                tracing::debug!("📖 Reader task exiting for {}", addr_str);
            });
            let addr_str2 = peer_addr.to_string();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let mut writer = w;
                while let Some(data) = write_rx.recv().await {
                    if let Err(e) = writer.write_all(&data).await {
                        tracing::debug!("📝 Write error for {}: {}", addr_str2, e);
                        break;
                    }
                    if let Err(e) = writer.flush().await {
                        tracing::debug!("📝 Flush error for {}: {}", addr_str2, e);
                        break;
                    }
                }
                tracing::debug!("📝 Writer task exiting for {}", addr_str2);
            });
        }

        Ok(Self {
            peer_ip,
            direction: ConnectionDirection::Inbound,
            msg_reader: msg_read_rx,
            writer_tx: write_tx,
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

    /// Get the writer channel sender for registration in peer registry
    pub fn shared_writer(&self) -> crate::network::peer_connection_registry::SharedPeerWriter {
        self.writer_tx.clone()
    }

    /// Send a message via the write channel
    fn send_message(
        writer_tx: &crate::network::peer_connection_registry::PeerWriterTx,
        message: &NetworkMessage,
    ) -> Result<(), String> {
        let frame_bytes = crate::network::wire::serialize_frame(message)?;
        writer_tx
            .send(frame_bytes)
            .map_err(|_| "Writer channel closed".to_string())
    }

    /// Send a ping to the peer
    async fn send_ping(&mut self, blockchain: Option<&Arc<Blockchain>>) -> Result<(), String> {
        let nonce = rand::random::<u64>();
        let timestamp = chrono::Utc::now().timestamp();
        let height = blockchain.map(|bc| bc.get_height());
        let direction = self.direction;
        let peer_ip = self.peer_ip.clone();

        {
            let mut state = self.ping_state.write().await;
            state.record_ping_sent(nonce);
        }

        let height_info = height
            .map(|h| format!(" at height {}", h))
            .unwrap_or_default();
        debug!(
            "📤 [{:?}] Sent ping to {}{} (nonce: {})",
            direction, peer_ip, height_info, nonce
        );

        Self::send_message(
            &self.writer_tx,
            &NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            },
        )
    }

    /// Handle received ping
    async fn handle_ping(
        &mut self,
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
        Self::send_message(
            &self.writer_tx,
            &NetworkMessage::Pong {
                nonce,
                timestamp,
                height: our_height,
            },
        )?;

        info!(
            "✅ [{:?}] Sent pong to {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        Ok(())
    }

    /// Handle received pong
    async fn handle_pong(
        &mut self,
        nonce: u64,
        _timestamp: i64,
        peer_height: Option<u64>,
    ) -> Result<(), String> {
        // Phase 3: Log peer height from pong
        let height_info = peer_height
            .map(|h| format!(" at height {}", h))
            .unwrap_or_default();
        debug!(
            "📨 [{:?}] Received pong from {}{}  (nonce: {})",
            self.direction, self.peer_ip, height_info, nonce
        );

        // Phase 3: Update peer height if provided
        if let Some(height) = peer_height {
            *self.peer_height.write().await = Some(height);
        }

        let mut state = self.ping_state.write().await;

        debug!(
            "📊 [{:?}] Before pong: {} pending pings, {} missed",
            self.direction,
            state.pending_pings.len(),
            state.missed_pongs
        );

        if state.record_pong_received(nonce) {
            debug!(
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
        &mut self,
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

    /// Unified message handler that delegates to MessageHandler
    /// This replaces the old handle_message_with_* functions
    async fn handle_message_unified(
        &mut self,
        message: NetworkMessage,
        config: &MessageLoopConfig,
    ) -> Result<(), String> {
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

            // Use from_registry to automatically fetch consensus engine
            let mut ctx = MessageContext::from_registry(
                Arc::clone(blockchain),
                Arc::clone(&config.peer_registry),
                Arc::clone(masternode_registry),
            )
            .await;

            // Add blacklist if available
            if let Some(ref blacklist) = config.blacklist {
                ctx = ctx.with_blacklist(Arc::clone(blacklist));
            }

            // Add AI system if available
            if let Some(ref ai_system) = config.ai_system {
                ctx = ctx.with_ai_system(Arc::clone(ai_system));
            }

            ctx
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
                if let Err(e) = Self::send_message(&self.writer_tx, &response) {
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

        if let Err(e) = Self::send_message(&self.writer_tx, &handshake) {
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
                // Read incoming messages from I/O bridge channel
                result = self.msg_reader.recv() => {
                    let result = match result {
                        Some(r) => r,
                        None => {
                            info!("🔌 [{:?}] Reader channel closed for {}", self.direction, self.peer_ip);
                            break;
                        }
                    };
                    match result {
                        Ok(None) => {
                            info!("🔌 [{:?}] Connection closed by {}", self.direction, self.peer_ip);
                            break;
                        }
                        Ok(Some(message)) => {
                            // Use unified message handler
                            let handle_result = self.handle_message_unified(message, &config).await;

                            if let Err(e) = handle_result {
                                warn!("⚠️ [{:?}] Error handling message from {}: {}",
                                      self.direction, self.peer_ip, e);
                            }
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
                        if let Err(e) = Self::send_message(&self.writer_tx, &msg) {
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
