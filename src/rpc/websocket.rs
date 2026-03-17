//! WebSocket server for real-time wallet notifications.
//!
//! Provides instant push notifications when transactions involving
//! subscribed addresses enter the mempool or get finalized by consensus.
//!
//! Protocol:
//!   Client → Server: {"method":"subscribe","params":{"address":"TIME0..."}}
//!   Client → Server: {"method":"unsubscribe","params":{"address":"TIME0..."}}
//!   Server → Client: {"type":"tx_notification","data":{...}}   (mempool entry — pending)
//!   Server → Client: {"type":"utxo_finalized","data":{...}}    (consensus reached — approved)
//!   Server → Client: {"type":"tx_declined","data":{...}}       (rejected during finality)
//!   Server → Client: {"type":"pong"}
//!   Client → Server: {"method":"ping"}

use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

/// Maximum concurrent WebSocket connections. When reached, new connections
/// receive an HTTP 503 with a JSON body so the wallet can failover to another node.
const MAX_WS_CONNECTIONS: usize = 5_000;

/// RAII guard that decrements the connection counter when dropped.
struct ConnGuard(Arc<AtomicUsize>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Transaction lifecycle status for WebSocket events
#[derive(Clone, Debug)]
pub enum TxEventStatus {
    /// Transaction entered the mempool (pending confirmation)
    Pending,
    /// Transaction reached consensus finality (approved)
    Finalized,
    /// Transaction was declined during finality
    Declined(String),
    /// A payment request targeting this address
    PaymentRequest {
        from_address: String,
        memo: String,
        pubkey_hex: String,
        expires: i64,
    },
}

/// Event emitted for transaction lifecycle (mempool entry, finalization, or decline)
#[derive(Clone, Debug, Serialize)]
pub struct TransactionEvent {
    pub txid: String,
    pub outputs: Vec<TxOutputInfo>,
    pub timestamp: i64,
    #[serde(skip)]
    pub status: TxEventStatus,
}

#[derive(Clone, Debug, Serialize)]
pub struct TxOutputInfo {
    pub address: String,
    pub amount: f64, // in TIME (value / 100_000_000)
    pub index: u32,
}

/// Message from client to server
#[derive(Deserialize, Debug)]
struct ClientMessage {
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// Notification sent to client
#[derive(Serialize, Debug)]
struct ServerNotification {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

/// Manages WebSocket subscriptions: address → list of notification senders
pub struct SubscriptionManager {
    subscriptions: DashMap<String, Vec<mpsc::UnboundedSender<ServerNotification>>>,
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            subscriptions: DashMap::new(),
        }
    }

    /// Subscribe a client to an address
    fn subscribe(&self, address: &str, sender: mpsc::UnboundedSender<ServerNotification>) {
        self.subscriptions
            .entry(address.to_string())
            .or_default()
            .push(sender);
        tracing::info!(
            "📡 WebSocket: client subscribed to {} (total subs: {})",
            address,
            self.total_subscriptions()
        );
    }

    /// Unsubscribe a client from an address (by removing closed senders)
    fn unsubscribe(&self, address: &str) {
        if let Some(mut senders) = self.subscriptions.get_mut(address) {
            senders.retain(|s| !s.is_closed());
            if senders.is_empty() {
                drop(senders);
                self.subscriptions.remove(address);
            }
        }
    }

    /// Notify all subscribers for affected addresses
    pub fn notify_transaction(&self, event: &TransactionEvent) {
        let msg_type = match &event.status {
            TxEventStatus::Finalized => "utxo_finalized",
            TxEventStatus::Pending => "tx_notification",
            TxEventStatus::Declined(_) => "tx_declined",
            TxEventStatus::PaymentRequest { .. } => "payment_request",
        };

        let sub_count = self.total_subscriptions();
        let sub_addrs: Vec<String> = self.subscriptions.iter().map(|e| e.key().clone()).collect();
        tracing::info!(
            "📡 WS notify_transaction: type={}, txid={}..., {} outputs, {} active subscriptions, subscribed addrs: {:?}",
            msg_type,
            &event.txid[..std::cmp::min(16, event.txid.len())],
            event.outputs.len(),
            sub_count,
            sub_addrs,
        );

        for output in &event.outputs {
            tracing::info!(
                "📡 WS checking output: address={}, subscribed={}",
                &output.address,
                self.subscriptions.contains_key(&output.address),
            );
            if let Some(senders) = self.subscriptions.get(&output.address) {
                let data = match &event.status {
                    TxEventStatus::Finalized => {
                        serde_json::json!({
                            "txid": event.txid,
                            "address": output.address,
                            "amount": output.amount,
                            "output_index": output.index,
                            "timestamp": event.timestamp,
                        })
                    }
                    TxEventStatus::Pending => {
                        serde_json::json!({
                            "txid": event.txid,
                            "address": output.address,
                            "amount": output.amount,
                            "output_index": output.index,
                            "timestamp": event.timestamp,
                            "confirmations": 0,
                        })
                    }
                    TxEventStatus::Declined(reason) => {
                        serde_json::json!({
                            "txid": event.txid,
                            "address": output.address,
                            "amount": output.amount,
                            "output_index": output.index,
                            "timestamp": event.timestamp,
                            "reason": reason,
                        })
                    }
                    TxEventStatus::PaymentRequest {
                        from_address,
                        memo,
                        pubkey_hex,
                        expires,
                    } => {
                        serde_json::json!({
                            "id": event.txid.strip_prefix("pr:").unwrap_or(&event.txid),
                            "from_address": from_address,
                            "to_address": output.address,
                            "amount": output.amount,
                            "memo": memo,
                            "pubkey": pubkey_hex,
                            "timestamp": event.timestamp,
                            "expires": expires,
                        })
                    }
                };

                let notification = ServerNotification {
                    msg_type: msg_type.to_string(),
                    data: Some(data),
                };

                for sender in senders.iter() {
                    let _ = sender.send(notification.clone());
                }
            }
        }
    }

    /// Notify all subscribers about a rejected transaction
    pub fn notify_rejection(&self, txid: &str, reason: &str) {
        let notification = ServerNotification {
            msg_type: "tx_rejected".to_string(),
            data: Some(serde_json::json!({
                "txid": txid,
                "reason": reason,
            })),
        };

        // Broadcast to all connected clients (sender may not be in outputs)
        let mut notified = std::collections::HashSet::new();
        for entry in self.subscriptions.iter() {
            for sender in entry.value().iter() {
                let ptr = sender as *const _ as usize;
                if notified.insert(ptr) {
                    let _ = sender.send(notification.clone());
                }
            }
        }

        tracing::info!(
            "📡 WS tx_rejected: txid={}..., reason={}, notified {} client(s)",
            &txid[..std::cmp::min(16, txid.len())],
            reason,
            notified.len(),
        );
    }

    /// Clean up dead connections
    fn cleanup_dead(&self) {
        let mut empty_keys = Vec::new();
        for mut entry in self.subscriptions.iter_mut() {
            entry.value_mut().retain(|s| !s.is_closed());
            if entry.value().is_empty() {
                empty_keys.push(entry.key().clone());
            }
        }
        for key in empty_keys {
            self.subscriptions.remove(&key);
        }
    }

    fn total_subscriptions(&self) -> usize {
        self.subscriptions.iter().map(|e| e.value().len()).sum()
    }

    pub fn active_connections(&self) -> usize {
        let mut unique = std::collections::HashSet::new();
        for entry in self.subscriptions.iter() {
            for sender in entry.value() {
                // Use pointer address as unique ID
                unique.insert(sender as *const _ as usize);
            }
        }
        unique.len()
    }
}

impl Clone for ServerNotification {
    fn clone(&self) -> Self {
        Self {
            msg_type: self.msg_type.clone(),
            data: self.data.clone(),
        }
    }
}

/// Start the WebSocket server
pub async fn start_ws_server(
    addr: &str,
    tx_events: broadcast::Sender<TransactionEvent>,
    shutdown: tokio_util::sync::CancellationToken,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    let sub_manager = Arc::new(SubscriptionManager::new());
    let conn_count = Arc::new(AtomicUsize::new(0));
    let scheme = if tls_acceptor.is_some() { "wss" } else { "ws" };

    println!("  ✅ WebSocket server listening on {}://{}", scheme, addr);

    // Spawn cleanup task (every 15s, remove dead connections)
    let cleanup_mgr = sub_manager.clone();
    let cleanup_shutdown = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {
                    cleanup_mgr.cleanup_dead();
                }
                _ = cleanup_shutdown.cancelled() => break,
            }
        }
    });

    // Spawn transaction event dispatcher (mempool notifications)
    let event_mgr = sub_manager.clone();
    let mut event_rx = tx_events.subscribe();
    let event_shutdown = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = event_rx.recv() => {
                    match result {
                        Ok(event) => {
                            let status_str = match &event.status {
                                TxEventStatus::Pending => "pending",
                                TxEventStatus::Finalized => "finalized",
                                TxEventStatus::Declined(_) => "declined",
                                TxEventStatus::PaymentRequest { .. } => "payment_request",
                            };
                            tracing::info!(
                                "📡 WS dispatcher received event: txid={}..., status={}",
                                &event.txid[..std::cmp::min(16, event.txid.len())],
                                status_str,
                            );
                            event_mgr.notify_transaction(&event);
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("WebSocket event dispatcher lagged by {} events", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = event_shutdown.cancelled() => break,
            }
        }
    });

    // Accept connections
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        let current = conn_count.load(Ordering::Relaxed);
                        let mgr = sub_manager.clone();
                        let conn_shutdown = shutdown.clone();
                        let cc = conn_count.clone();
                        if let Some(ref acceptor) = tls_acceptor {
                            let acceptor = acceptor.clone();
                            tokio::spawn(async move {
                                match acceptor.accept(stream).await {
                                    Ok(tls_stream) => {
                                        dispatch_connection(tls_stream, addr, current, mgr, conn_shutdown, cc).await;
                                    }
                                    Err(e) => {
                                        tracing::debug!("TLS handshake failed from {}: {}", addr, e);
                                    }
                                }
                            });
                        } else {
                            tokio::spawn(async move {
                                dispatch_connection(stream, addr, current, mgr, conn_shutdown, cc).await;
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!("WebSocket accept error: {}", e);
                    }
                }
            }
            _ = shutdown.cancelled() => {
                tracing::info!("🛑 WebSocket server shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Check capacity and either reject with HTTP 503 or hand off to `handle_connection`.
async fn dispatch_connection<S>(
    stream: S,
    addr: std::net::SocketAddr,
    current: usize,
    sub_manager: Arc<SubscriptionManager>,
    shutdown: tokio_util::sync::CancellationToken,
    conn_count: Arc<AtomicUsize>,
) where
    S: tokio::io::AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if current >= MAX_WS_CONNECTIONS {
        tracing::warn!(
            "⚠ WS connection from {} rejected — at capacity ({}/{})",
            addr,
            current,
            MAX_WS_CONNECTIONS
        );
        let body =
            r#"{"error":"capacity","message":"Server at capacity. Try another masternode."}"#;
        let response = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(), body
        );
        let mut s = stream;
        let _ = s.write_all(response.as_bytes()).await;
    } else {
        conn_count.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            "📡 WebSocket connection from {} ({}/{})",
            addr,
            current + 1,
            MAX_WS_CONNECTIONS
        );
        if let Err(e) = handle_connection(stream, sub_manager, shutdown, conn_count).await {
            tracing::debug!("WebSocket connection error from {}: {}", addr, e);
        }
    }
}

async fn handle_connection<S>(
    stream: S,
    sub_manager: Arc<SubscriptionManager>,
    shutdown: tokio_util::sync::CancellationToken,
    conn_count: Arc<AtomicUsize>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: tokio::io::AsyncRead + AsyncWrite + Unpin,
{
    // Auto-decrement the connection counter when this function exits for any reason
    let _guard = ConnGuard(conn_count);
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Channel for sending notifications to this client
    let (notif_tx, mut notif_rx) = mpsc::unbounded_channel::<ServerNotification>();
    let mut subscribed_addresses: Vec<String> = Vec::new();

    // Heartbeat: send ping every 30s
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));

    loop {
        tokio::select! {
            // Incoming message from client
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) {
                            match client_msg.method.as_str() {
                                "subscribe" => {
                                    if let Some(addr) = client_msg.params.get("address").and_then(|v| v.as_str()) {
                                        sub_manager.subscribe(addr, notif_tx.clone());
                                        subscribed_addresses.push(addr.to_string());
                                        let resp = ServerNotification {
                                            msg_type: "subscribed".to_string(),
                                            data: Some(serde_json::json!({"address": addr})),
                                        };
                                        let json = serde_json::to_string(&resp)?;
                                        ws_sender.send(Message::Text(json.into())).await?;
                                    }
                                }
                                "unsubscribe" => {
                                    if let Some(addr) = client_msg.params.get("address").and_then(|v| v.as_str()) {
                                        sub_manager.unsubscribe(addr);
                                        subscribed_addresses.retain(|a| a != addr);
                                        let resp = ServerNotification {
                                            msg_type: "unsubscribed".to_string(),
                                            data: Some(serde_json::json!({"address": addr})),
                                        };
                                        let json = serde_json::to_string(&resp)?;
                                        ws_sender.send(Message::Text(json.into())).await?;
                                    }
                                }
                                "ping" => {
                                    let resp = ServerNotification {
                                        msg_type: "pong".to_string(),
                                        data: None,
                                    };
                                    let json = serde_json::to_string(&resp)?;
                                    ws_sender.send(Message::Text(json.into())).await?;
                                }
                                _ => {
                                    tracing::debug!("Unknown WebSocket method: {}", client_msg.method);
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        ws_sender.send(Message::Pong(data)).await?;
                    }
                    Some(Err(e)) => {
                        tracing::debug!("WebSocket receive error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }

            // Outgoing notification to client
            Some(notification) = notif_rx.recv() => {
                let json = serde_json::to_string(&notification)?;
                if ws_sender.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }

            // Heartbeat ping
            _ = heartbeat.tick() => {
                if ws_sender.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }

            // Shutdown
            _ = shutdown.cancelled() => break,
        }
    }

    // Clean up subscriptions for this connection
    for addr in &subscribed_addresses {
        sub_manager.unsubscribe(addr);
    }

    Ok(())
}
