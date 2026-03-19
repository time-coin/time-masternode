//! TCP-based JSON-RPC server for masternode communication.
//!
//! This is the primary RPC server used by the masternode. It listens on a
//! configurable address (default `0.0.0.0:{rpc_port}`) and serves JSON-RPC
//! 2.0 requests over TCP with HTTP framing.
//!
//! Public (read-only) methods are accessible without authentication.
//! State-modifying methods require HTTP Basic Auth credentials.

use super::handler::RpcHandler;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Serialize, Deserialize, Debug)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: String,
    pub method: String,
    pub params: Value,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Parsed rpcauth entry: user:salt$hash
#[derive(Clone)]
struct RpcAuthEntry {
    user: String,
    salt: String,
    hash: String,
}

/// RPC authenticator supporting plaintext and hashed (rpcauth) credentials.
#[derive(Clone)]
struct RpcAuthenticator {
    /// Base64-encoded "user:password" for plaintext auth. Empty = disabled.
    plaintext_token: String,
    /// Hashed credential entries (Bitcoin-style rpcauth=user:salt$hash)
    hashed_entries: Vec<RpcAuthEntry>,
}

impl RpcAuthenticator {
    fn new(rpcuser: &str, rpcpassword: &str, rpcauth_lines: &[String]) -> Self {
        let plaintext_token = if !rpcuser.is_empty() && !rpcpassword.is_empty() {
            base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", rpcuser, rpcpassword))
        } else {
            String::new()
        };

        let hashed_entries: Vec<RpcAuthEntry> = rpcauth_lines
            .iter()
            .filter_map(|line| {
                // Format: user:salt$hash
                let (user, rest) = line.split_once(':')?;
                let (salt, hash) = rest.split_once('$')?;
                Some(RpcAuthEntry {
                    user: user.to_string(),
                    salt: salt.to_string(),
                    hash: hash.to_string(),
                })
            })
            .collect();

        Self {
            plaintext_token,
            hashed_entries,
        }
    }

    fn is_enabled(&self) -> bool {
        !self.plaintext_token.is_empty() || !self.hashed_entries.is_empty()
    }

    /// Check if the provided Basic Auth credentials are valid.
    fn check(&self, auth_header: &str) -> bool {
        let token = match auth_header.strip_prefix("Basic ") {
            Some(t) => t.trim(),
            None => return false,
        };

        // Check plaintext credentials first
        if !self.plaintext_token.is_empty() && token == self.plaintext_token {
            return true;
        }

        // Decode base64 to get "user:password"
        let decoded = match base64::engine::general_purpose::STANDARD.decode(token) {
            Ok(d) => d,
            Err(_) => return false,
        };
        let credentials = match String::from_utf8(decoded) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let (user, password) = match credentials.split_once(':') {
            Some(pair) => pair,
            None => return false,
        };

        // Check against hashed entries
        for entry in &self.hashed_entries {
            if entry.user == user {
                // HMAC-SHA256(key=salt, message=password)
                if let Ok(mut mac) = HmacSha256::new_from_slice(entry.salt.as_bytes()) {
                    mac.update(password.as_bytes());
                    let result = hex::encode(mac.finalize().into_bytes());
                    if result == entry.hash {
                        return true;
                    }
                }
            }
        }

        false
    }
}

/// Per-IP RPC rate limiter. Allows `max_per_second` requests per IP per second.
struct RpcRateLimiter {
    /// Map of IP → (window_start, request_count)
    counters: DashMap<std::net::IpAddr, (Instant, u32)>,
    max_per_second: u32,
}

impl RpcRateLimiter {
    fn new(max_per_second: u32) -> Self {
        Self {
            counters: DashMap::new(),
            max_per_second,
        }
    }

    /// Returns true if the request is allowed, false if rate-limited.
    fn check(&self, addr: std::net::IpAddr) -> bool {
        let now = Instant::now();
        let mut entry = self.counters.entry(addr).or_insert((now, 0));
        let (window_start, count) = entry.value_mut();

        if now.duration_since(*window_start).as_secs() >= 1 {
            // New window
            *window_start = now;
            *count = 1;
            true
        } else if *count < self.max_per_second {
            *count += 1;
            true
        } else {
            false
        }
    }

    /// Periodic cleanup of stale entries (call every ~60s)
    fn cleanup(&self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(60);
        self.counters.retain(|_, (t, _)| *t > cutoff);
    }
}

/// Read-only RPC methods that do not require authentication.
///
/// These methods only query blockchain state and never modify it, so they are
/// safe to expose to unauthenticated callers (e.g. light wallets).
const PUBLIC_METHODS: &[&str] = &[
    // Blockchain queries
    "getblockchaininfo",
    "getblockcount",
    "getblock",
    "getbestblockhash",
    "getblockhash",
    "getnetworkinfo",
    "getpeerinfo",
    "gettxoutsetinfo",
    "getinfo",
    "uptime",
    // Transaction queries
    "getrawtransaction",
    "gettransaction",
    "gettransactions",
    "decoderawtransaction",
    "gettransactionfinality",
    "waittransactionfinality",
    // Transaction submission (signature is the authentication)
    "sendrawtransaction",
    // Wallet / balance queries
    "getbalance",
    "getbalances",
    "listunspent",
    "listunspentmulti",
    "listtransactions",
    "listtransactionsmulti",
    "listreceivedbyaddress",
    "getwalletinfo",
    "validateaddress",
    // UTXO / mempool queries
    "getmempoolinfo",
    "getrawmempool",
    "getmempoolverbose",
    "gettxindexstatus",
    "listlockedutxos",
    "listlockedcollaterals",
    // Masternode queries
    "masternodelist",
    "masternodestatus",
    "masternodereginfo",
    "masternoderegstatus",
    // Consensus / network queries
    "getconsensusinfo",
    "gettimevotestatus",
    "getwhitelist",
    "getblacklist",
    "gettreasurybalance",
    "getfeeschedule",
    // Payment requests — signed by the requester/payer; signature is the authentication
    "sendpaymentrequest",
    "getpaymentrequests",
    "acknowledgepaymentrequest",
    "respondpaymentrequest",
    "cancelpaymentrequest",
    "markpaymentrequestviewed",
];

/// Returns `true` if the given RPC method is in the public (read-only) whitelist
/// and may be called without authentication.
fn is_public_method(method: &str) -> bool {
    PUBLIC_METHODS.contains(&method)
}

pub struct RpcServer {
    listener: TcpListener,
    handler: Arc<RpcHandler>,
    auth: Arc<RpcAuthenticator>,
    rate_limiter: Arc<RpcRateLimiter>,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
}

impl RpcServer {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        addr: &str,
        consensus: Arc<ConsensusEngine>,
        utxo_manager: Arc<UTXOStateManager>,
        network: NetworkType,
        registry: Arc<MasternodeRegistry>,
        blockchain: Arc<crate::blockchain::Blockchain>,
        blacklist: Arc<tokio::sync::RwLock<crate::network::blacklist::IPBlacklist>>,
        tx_event_sender: Option<tokio::sync::broadcast::Sender<super::websocket::TransactionEvent>>,
        rpcuser: String,
        rpcpassword: String,
        rpcauth: Vec<String>,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        let mut handler = RpcHandler::new(
            consensus,
            utxo_manager,
            network,
            registry,
            blockchain,
            blacklist,
        );
        if let Some(sender) = tx_event_sender {
            handler.set_tx_event_sender(sender);
        }

        let auth = RpcAuthenticator::new(&rpcuser, &rpcpassword, &rpcauth);

        Ok(Self {
            listener,
            handler: Arc::new(handler),
            auth: Arc::new(auth),
            rate_limiter: Arc::new(RpcRateLimiter::new(100)),
            tls_acceptor: None,
        })
    }

    /// Enable TLS for the RPC server with the given acceptor.
    pub fn set_tls(&mut self, acceptor: tokio_rustls::TlsAcceptor) {
        self.tls_acceptor = Some(acceptor);
    }

    pub async fn run(&mut self) -> Result<(), std::io::Error> {
        let auth_mode = if !self.auth.is_enabled() {
            "disabled"
        } else if !self.auth.hashed_entries.is_empty() {
            "enabled (rpcauth)"
        } else {
            "enabled"
        };
        let tls_mode = if self.tls_acceptor.is_some() {
            "TLS + plain (auto-detect)"
        } else {
            "plain"
        };
        println!(
            "  ✅ RPC server listening on {} (auth: {}, transport: {})",
            self.listener.local_addr()?,
            auth_mode,
            tls_mode,
        );

        // Spawn periodic rate limiter cleanup
        let cleanup_limiter = self.rate_limiter.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                cleanup_limiter.cleanup();
            }
        });

        loop {
            let (socket, addr) = self.listener.accept().await?;

            // Rate limit check (before TLS handshake to save resources)
            if !self.rate_limiter.check(addr.ip()) {
                // Best-effort plain error; if client is doing TLS we can't respond before
                // the handshake, so just drop it.
                let _ =
                    Self::send_error_tcp(socket, "Rate limit exceeded. Try again later.", -32005)
                        .await;
                continue;
            }

            let handler = self.handler.clone();
            let auth = self.auth.clone();
            let tls = self.tls_acceptor.clone();

            tokio::spawn(async move {
                if let Some(acceptor) = tls {
                    // Peek the first byte to auto-detect TLS vs plain HTTP.
                    // TLS ClientHello always starts with 0x16 (22).
                    // Plain HTTP starts with an ASCII method byte (e.g. 'P' = 0x50).
                    // Accepting both on the same port avoids "tls handshake eof" noise
                    // from clients that connect with http:// instead of https://.
                    let mut peek = [0u8; 1];
                    match socket.peek(&mut peek).await {
                        Ok(1) if peek[0] == 0x16 => {
                            // TLS ClientHello — do the handshake then dispatch
                            match acceptor.accept(socket).await {
                                Ok(tls_stream) => {
                                    if let Err(e) =
                                        Self::handle_connection(tls_stream, handler, &auth).await
                                    {
                                        // Suppress the rustls "peer closed without close_notify"
                                        // noise — this is benign and fired by clients/proxies that
                                        // don't send a TLS close_notify before dropping the TCP
                                        // connection (browsers, curl, health checks, etc.).
                                        let msg = e.to_string();
                                        if !msg.contains("close_notify") {
                                            eprintln!("RPC TLS connection error: {}", e);
                                        }
                                    }
                                }
                                Err(_e) => {
                                    // Handshake failure (e.g. client sent plain HTTP) — ignore
                                }
                            }
                        }
                        Ok(1) => {
                            // Plain HTTP on a TLS-enabled port — serve directly
                            if let Err(e) = Self::handle_connection(socket, handler, &auth).await {
                                eprintln!("RPC plain connection error from {}: {}", addr, e);
                            }
                        }
                        _ => {} // connection closed before first byte — ignore silently
                    }
                } else if let Err(e) = Self::handle_connection(socket, handler, &auth).await {
                    eprintln!("RPC error: {}", e);
                }
            });
        }
    }

    /// Send a JSON-RPC error response over a plain TCP connection.
    async fn send_error_tcp(
        mut socket: tokio::net::TcpStream,
        message: &str,
        code: i32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response = RpcResponse {
            jsonrpc: "2.0".to_string(),
            id: "rate-limited".to_string(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.to_string(),
            }),
        };
        let json = serde_json::to_string(&response)?;
        let http = format!(
            "HTTP/1.1 429 Too Many Requests\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            json.len(),
            json
        );
        socket.write_all(http.as_bytes()).await?;
        socket.flush().await?;
        Ok(())
    }

    async fn handle_connection<S: AsyncReadExt + AsyncWriteExt + Unpin>(
        mut socket: S,
        handler: Arc<RpcHandler>,
        auth: &RpcAuthenticator,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Read the full HTTP request, handling TCP fragmentation.
        // We parse Content-Length from headers to know how many body bytes to expect.
        const MAX_REQUEST_SIZE: usize = 16_777_216; // 16 MB — large txs consolidate many UTXOs
        let mut data = Vec::with_capacity(65536);
        let mut tmp = [0u8; 65536];

        loop {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&tmp[..n]);

            if data.len() > MAX_REQUEST_SIZE {
                break;
            }

            // Check if we have the full request:
            // 1. Find the header/body boundary
            // 2. Parse Content-Length
            // 3. Check if we've received enough body bytes
            let header_end = data
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .or_else(|| data.windows(2).position(|w| w == b"\n\n").map(|p| p + 2));

            if let Some(body_offset) = header_end {
                let headers = String::from_utf8_lossy(&data[..body_offset]);
                let content_length = headers
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split_once(':'))
                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);

                let body_received = data.len() - body_offset;
                if body_received >= content_length {
                    break; // We have the full request
                }
            }
        }

        if data.is_empty() {
            return Ok(());
        }

        let http_request = String::from_utf8_lossy(&data);

        // Extract JSON body from HTTP POST request
        let body = if let Some(body_start) = http_request.find("\r\n\r\n") {
            &http_request[body_start + 4..]
        } else if let Some(body_start) = http_request.find("\n\n") {
            &http_request[body_start + 2..]
        } else {
            ""
        };

        let response = if body.is_empty() {
            // Invalid request
            RpcResponse {
                jsonrpc: "2.0".to_string(),
                id: "unknown".to_string(),
                result: None,
                error: Some(RpcError {
                    code: -32700,
                    message: "No request body".to_string(),
                }),
            }
        } else {
            match serde_json::from_str::<RpcRequest>(body.trim_end_matches('\0')) {
                Ok(request) => {
                    // Only require auth for non-public (state-modifying) methods
                    if auth.is_enabled() && !is_public_method(&request.method) {
                        let authorized = http_request
                            .lines()
                            .find(|line| {
                                let lower = line.to_lowercase();
                                lower.starts_with("authorization:")
                            })
                            .and_then(|line| {
                                line.split_once(':').map(|(_, v)| v.trim().to_string())
                            })
                            .map(|value| auth.check(&value))
                            .unwrap_or(false);

                        if !authorized {
                            let error_response = RpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: request.id,
                                result: None,
                                error: Some(RpcError {
                                    code: -32600,
                                    message: "Unauthorized: invalid or missing RPC credentials"
                                        .to_string(),
                                }),
                            };
                            let error_json = serde_json::to_string(&error_response)?;
                            let http_response = format!(
                                "HTTP/1.1 401 Unauthorized\r\n\
                                 WWW-Authenticate: Basic realm=\"TIME Coin RPC\"\r\n\
                                 Content-Type: application/json\r\n\
                                 Content-Length: {}\r\n\
                                 Connection: close\r\n\
                                 \r\n\
                                 {}",
                                error_json.len(),
                                error_json
                            );
                            socket.write_all(http_response.as_bytes()).await?;
                            socket.flush().await?;
                            return Ok(());
                        }
                    }

                    handler.handle_request(request).await
                }
                Err(e) => RpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: "unknown".to_string(),
                    result: None,
                    error: Some(RpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                    }),
                },
            }
        };

        let response_json = serde_json::to_string(&response)?;

        // Send HTTP response
        let http_response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {}",
            response_json.len(),
            response_json
        );

        socket.write_all(http_response.as_bytes()).await?;
        socket.flush().await?;

        Ok(())
    }
}
