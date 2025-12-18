use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, Instant};
use tracing::{debug, error, info, warn};

use crate::network::message::NetworkMessage;

// Allow dead code during refactor
#[allow(dead_code)]
/// Direction of connection establishment
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnectionDirection {
    /// They connected to us
    Inbound,
    /// We connected to them
    Outbound,
}

/// State for tracking ping/pong health
#[allow(dead_code)]
#[derive(Debug)]
struct PingState {
    last_ping_sent: Option<Instant>,
    last_pong_received: Option<Instant>,
    pending_pings: Vec<(u64, Instant)>, // (nonce, sent_time)
    missed_pongs: u32,
}

#[allow(dead_code)]
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
#[allow(dead_code)]
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

    /// Local listening port (for logging)
    local_port: u16,

    /// Remote port for this connection (ephemeral)
    remote_port: u16,
}

#[allow(dead_code)]
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
            reader: BufReader::new(read_half),
            writer: Arc::new(Mutex::new(BufWriter::new(write_half))),
            ping_state: Arc::new(RwLock::new(PingState::new())),
            local_port: local_addr.port(),
            remote_port: remote_addr.port(),
        })
    }

    /// Create a new inbound connection from a peer
    pub async fn new_inbound(stream: TcpStream) -> Result<Self, String> {
        let peer_addr = stream
            .peer_addr()
            .map_err(|e| format!("Failed to get peer address: {}", e))?;

        let local_addr = stream
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?;

        let peer_ip = peer_addr.ip().to_string();

        info!("🔗 [INBOUND] Accepted connection from {}", peer_addr);

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            peer_ip,
            direction: ConnectionDirection::Inbound,
            reader: BufReader::new(read_half),
            writer: Arc::new(Mutex::new(BufWriter::new(write_half))),
            ping_state: Arc::new(RwLock::new(PingState::new())),
            local_port: local_addr.port(),
            remote_port: peer_addr.port(),
        })
    }

    /// Get peer IP (identity)
    pub fn peer_ip(&self) -> &str {
        &self.peer_ip
    }

    /// Get connection direction
    pub fn direction(&self) -> ConnectionDirection {
        self.direction
    }

    /// Get remote port for this connection
    pub fn remote_port(&self) -> u16 {
        self.remote_port
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
            "📨 [{:?}] RECEIVED PING from {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        let timestamp = chrono::Utc::now().timestamp();
        self.send_message(&NetworkMessage::Pong { nonce, timestamp })
            .await?;

        info!(
            "✅ [{:?}] SENT PONG to {} (nonce: {})",
            self.direction, self.peer_ip, nonce
        );

        Ok(())
    }

    /// Handle received pong
    async fn handle_pong(&self, nonce: u64, _timestamp: i64) -> Result<(), String> {
        info!(
            "📨 [{:?}] RECEIVED PONG from {} (nonce: {})",
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
    async fn should_disconnect(&self) -> bool {
        let mut state = self.ping_state.write().await;

        if state.check_timeout(Self::MAX_MISSED_PONGS, Self::PONG_TIMEOUT) {
            warn!(
                "⚠️ [{:?}] Peer {} unresponsive after {} missed pongs",
                self.direction, self.peer_ip, state.missed_pongs
            );
            true
        } else {
            false
        }
    }

    /// Run the unified message loop for this connection
    pub async fn run_message_loop(mut self) -> Result<(), String> {
        let mut ping_interval = interval(Self::PING_INTERVAL);
        let mut timeout_check = interval(Self::TIMEOUT_CHECK_INTERVAL);
        let mut buffer = String::new();

        info!(
            "🔄 [{:?}] Starting message loop for {} (port: {})",
            self.direction, self.peer_ip, self.remote_port
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

                // Check for timeout
                _ = timeout_check.tick() => {
                    if self.should_disconnect().await {
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
                // Other message types not handled by PeerConnection yet
                // TODO: Extend PeerConnection to handle other message types
            }
        }

        Ok(())
    }
}
