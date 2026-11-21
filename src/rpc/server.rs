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
    /// JSON-RPC 2.0 id — may be a string, integer, or null per spec.
    #[serde(default)]
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Value,
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
use subtle::ConstantTimeEq;

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

        // Check plaintext credentials first (constant-time to avoid a timing oracle)
        if !self.plaintext_token.is_empty()
            && bool::from(token.as_bytes().ct_eq(self.plaintext_token.as_bytes()))
        {
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
                    if bool::from(result.as_bytes().ct_eq(entry.hash.as_bytes())) {
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
///
/// SECURITY: never add a method here that reads or acts with the node's own
/// wallet key or private data (e.g. `getmessages` decrypts the operator's
/// inbox, `listcontacts` leaks the address book, `getwalletinfo` does a full
/// UTXO scan of the node wallet, `acknowledgepaymentrequest` deletes requests
/// without any signature). Those must stay behind RPC credentials.
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
    "rebroadcasttransaction",
    // Wallet / balance queries
    "getbalance",
    "getbalances",
    "listunspent",
    "listunspentmulti",
    "listtransactions",
    "listtransactionsmulti",
    "listreceivedbyaddress",
    "validateaddress",
    // UTXO / mempool queries
    "getmempoolinfo",
    "getrawmempool",
    "getmempoolverbose",
    "gettxindexstatus",
    "listlockedutxos",
    "listlockedcollaterals",
    "getrewardreport",
    "findblockbydate",
    // Masternode queries
    "masternodelist",
    "masternodestatus",
    "masternodereginfo",
    "masternoderegstatus",
    // Consensus / network queries
    "getconsensusinfo",
    "gettimevotestatus",
    "getwhitelist",
    "getbanlist",
    "gettreasurybalance",
    "getfeeschedule",
    // Payment requests — signed by the requester/payer; signature is the authentication
    "sendpaymentrequest",
    "getpaymentrequests",
    "respondpaymentrequest",
    "cancelpaymentrequest",
    "markpaymentrequestviewed",
    // Secure messaging — read-only and thin-client methods
    // Thin-client envelope methods are self-authenticated by the sender's Ed25519 signature,
    // so they are safe to expose without HTTP-level credentials.
    "getpubkey",
    "getmessagestatus",
    "lookuppubkey",
    "registerpubkey",
    "registeraddresspubkey",
    "getrawenvelopes",
    "submitenvelope",
    "publishpubkey",
];

/// Returns `true` if the given RPC method is in the public (read-only) whitelist
/// and may be called without authentication.
fn is_public_method(method: &str) -> bool {
    PUBLIC_METHODS.contains(&method)
}

/// Max simultaneous RPC connections (all IPs). Bounds fds/memory under slowloris.
const MAX_RPC_CONNECTIONS: usize = 512;
/// Max simultaneous RPC connections from a single IP.
const MAX_RPC_CONNECTIONS_PER_IP: usize = 32;
/// Time allowed for the TLS handshake / first-byte peek.
const RPC_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Time allowed to receive the complete HTTP request.
const RPC_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Time allowed to write the response.
const RPC_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Normalise IPv4-mapped IPv6 (`::ffff:a.b.c.d`) to plain IPv4.
fn canonical_ip(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map(std::net::IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// Parse an `rpcallowip` entry: a bare IP or `ip/prefix` CIDR.
fn parse_allow_entry(entry: &str) -> Option<(std::net::IpAddr, u8)> {
    let entry = entry.trim();
    let (ip_str, prefix) = match entry.split_once('/') {
        Some((ip, p)) => (ip, Some(p.parse::<u8>().ok()?)),
        None => (entry, None),
    };
    let ip = canonical_ip(ip_str.parse::<std::net::IpAddr>().ok()?);
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let prefix = prefix.unwrap_or(max);
    if prefix > max {
        return None;
    }
    Some((ip, prefix))
}

fn ip_in_cidr(ip: std::net::IpAddr, net: std::net::IpAddr, prefix: u8) -> bool {
    use std::net::IpAddr;
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix as u32)
            };
            (u32::from(a) & mask) == (u32::from(b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix as u32)
            };
            (u128::from(a) & mask) == (u128::from(b) & mask)
        }
        _ => false,
    }
}

/// Releases the global slot and decrements the per-IP counter when a
/// connection task ends.
struct RpcConnGuard {
    ip: std::net::IpAddr,
    per_ip: Arc<DashMap<std::net::IpAddr, usize>>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for RpcConnGuard {
    fn drop(&mut self) {
        if let dashmap::mapref::entry::Entry::Occupied(mut e) = self.per_ip.entry(self.ip) {
            let v = e.get_mut();
            *v = v.saturating_sub(1);
            if *v == 0 {
                e.remove();
            }
        }
    }
}

pub struct RpcServer {
    listener: TcpListener,
    handler: Arc<RpcHandler>,
    auth: Arc<RpcAuthenticator>,
    rate_limiter: Arc<RpcRateLimiter>,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
    /// `rpcallowip` entries. Empty = no IP restriction. Loopback is always allowed.
    allow_ips: Vec<(std::net::IpAddr, u8)>,
    conn_slots: Arc<tokio::sync::Semaphore>,
    conns_per_ip: Arc<DashMap<std::net::IpAddr, usize>>,
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
        banlist: Arc<tokio::sync::RwLock<crate::network::banlist::IPBanlist>>,
        tx_event_sender: Option<tokio::sync::broadcast::Sender<super::websocket::TransactionEvent>>,
        reconnection_ai: Option<Arc<crate::ai::adaptive_reconnection::AdaptiveReconnectionAI>>,
        rpcuser: String,
        rpcpassword: String,
        rpcauth: Vec<String>,
        data_dir: String,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(addr).await?;
        let mut handler = RpcHandler::new(
            consensus,
            utxo_manager,
            network,
            registry,
            blockchain,
            banlist,
            data_dir,
        );
        if let Some(sender) = tx_event_sender {
            handler.set_tx_event_sender(sender);
        }
        if let Some(ai) = reconnection_ai {
            handler.set_reconnection_ai(ai);
        }

        let auth = RpcAuthenticator::new(&rpcuser, &rpcpassword, &rpcauth);

        Ok(Self {
            listener,
            handler: Arc::new(handler),
            auth: Arc::new(auth),
            rate_limiter: Arc::new(RpcRateLimiter::new(100)),
            tls_acceptor: None,
            allow_ips: Vec::new(),
            conn_slots: Arc::new(tokio::sync::Semaphore::new(MAX_RPC_CONNECTIONS)),
            conns_per_ip: Arc::new(DashMap::new()),
        })
    }

    /// Restrict RPC to the given `rpcallowip` entries (IPs or CIDRs).
    /// Loopback is always permitted so the local CLI/dashboard keep working.
    pub fn set_allow_ips(&mut self, entries: &[String]) {
        self.allow_ips = entries
            .iter()
            .filter_map(|e| {
                let parsed = parse_allow_entry(e);
                if parsed.is_none() {
                    eprintln!("  ⚠ Ignoring invalid rpcallowip entry: {}", e);
                }
                parsed
            })
            .collect();
    }

    fn ip_allowed(&self, ip: std::net::IpAddr) -> bool {
        if self.allow_ips.is_empty() || ip.is_loopback() {
            return true;
        }
        self.allow_ips
            .iter()
            .any(|(net, prefix)| ip_in_cidr(ip, *net, *prefix))
    }

    /// Enable TLS for the RPC server with the given acceptor.
    pub fn set_tls(&mut self, acceptor: tokio_rustls::TlsAcceptor) {
        self.tls_acceptor = Some(acceptor);
    }

    /// Wire the relay store and P2P registry for Silver/Gold relay nodes.
    /// `set_contacts_book` should be called first (on all nodes).
    /// Must be called before `run()`.
    pub fn set_messaging(
        &mut self,
        relay_store: Arc<crate::messaging::relay::RelayStore>,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) {
        if let Some(h) = Arc::get_mut(&mut self.handler) {
            h.set_relay_store(relay_store, peer_registry);
        }
    }

    /// Wire the contacts book into the RPC handler (available on all nodes).
    /// Must be called before `run()`.
    pub fn set_contacts_book(
        &mut self,
        contacts_book: Arc<crate::messaging::contacts::ContactsBook>,
    ) {
        if let Some(h) = Arc::get_mut(&mut self.handler) {
            h.set_contacts_book(contacts_book);
        }
    }

    /// Wire the P2P peer registry for all nodes (needed for pubkey resolution and message routing).
    /// Must be called before `run()`.
    pub fn set_peer_registry(
        &mut self,
        peer_registry: Arc<crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) {
        if let Some(h) = Arc::get_mut(&mut self.handler) {
            h.set_peer_registry(peer_registry);
        }
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

        if !self.allow_ips.is_empty() {
            println!(
                "  🔒 RPC restricted to {} rpcallowip entr{} + loopback",
                self.allow_ips.len(),
                if self.allow_ips.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
        }

        loop {
            // Never let a transient accept() error (e.g. EMFILE when fds are
            // exhausted) terminate the RPC server — back off and keep serving.
            let (socket, addr) = match self.listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("RPC accept error (retrying): {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    continue;
                }
            };
            let peer_ip = canonical_ip(addr.ip());

            if !self.ip_allowed(peer_ip) {
                drop(socket);
                continue;
            }

            // Rate limit check (before TLS handshake to save resources)
            if !self.rate_limiter.check(peer_ip) {
                // Best-effort plain error; if client is doing TLS we can't respond before
                // the handshake, so just drop it.
                let _ =
                    Self::send_error_tcp(socket, "Rate limit exceeded. Try again later.", -32005)
                        .await;
                continue;
            }

            // Concurrent-connection caps (global + per IP). Without these an
            // attacker can hold thousands of idle sockets and exhaust fds/memory.
            let permit = match self.conn_slots.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    drop(socket);
                    continue;
                }
            };
            {
                let mut count = self.conns_per_ip.entry(peer_ip).or_insert(0);
                if *count >= MAX_RPC_CONNECTIONS_PER_IP {
                    drop(count);
                    drop(socket);
                    continue;
                }
                *count += 1;
            }
            let guard = RpcConnGuard {
                ip: peer_ip,
                per_ip: self.conns_per_ip.clone(),
                _permit: permit,
            };

            let handler = self.handler.clone();
            let auth = self.auth.clone();
            let tls = self.tls_acceptor.clone();

            tokio::spawn(async move {
                let _guard = guard;
                if let Some(acceptor) = tls {
                    // Peek the first byte to auto-detect TLS vs plain HTTP.
                    // TLS ClientHello always starts with 0x16 (22).
                    // Plain HTTP starts with an ASCII method byte (e.g. 'P' = 0x50).
                    // Accepting both on the same port avoids "tls handshake eof" noise
                    // from clients that connect with http:// instead of https://.
                    let mut peek = [0u8; 1];
                    let peeked =
                        tokio::time::timeout(RPC_HANDSHAKE_TIMEOUT, socket.peek(&mut peek)).await;
                    match peeked {
                        Ok(Ok(1)) if peek[0] == 0x16 => {
                            // TLS ClientHello — do the handshake then dispatch
                            let accepted = tokio::time::timeout(
                                RPC_HANDSHAKE_TIMEOUT,
                                acceptor.accept(socket),
                            )
                            .await;
                            match accepted {
                                Ok(Ok(tls_stream)) => {
                                    if let Err(e) =
                                        Self::handle_connection(tls_stream, handler, &auth, peer_ip)
                                            .await
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
                                _ => {
                                    // Handshake failure or timeout — ignore
                                }
                            }
                        }
                        Ok(Ok(1)) => {
                            // Plain HTTP on a TLS-enabled port — serve directly
                            if let Err(e) =
                                Self::handle_connection(socket, handler, &auth, peer_ip).await
                            {
                                eprintln!("RPC plain connection error from {}: {}", addr, e);
                            }
                        }
                        _ => {} // closed / timed out before first byte — ignore silently
                    }
                } else if let Err(e) =
                    Self::handle_connection(socket, handler, &auth, peer_ip).await
                {
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
            id: Value::Null,
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

    /// Read one HTTP request (headers + Content-Length body), handling TCP
    /// fragmentation. The header terminator is searched only in newly received
    /// bytes so a large request costs O(n), not O(n²).
    async fn read_http_request<S: AsyncReadExt + Unpin>(
        socket: &mut S,
    ) -> Result<Vec<u8>, std::io::Error> {
        const MAX_REQUEST_SIZE: usize = 16_777_216; // 16 MB — large txs consolidate many UTXOs
        const MAX_HEADER_SIZE: usize = 64 * 1024;
        let mut data = Vec::with_capacity(8192);
        let mut tmp = [0u8; 65536];
        // (body_offset, content_length) once headers are complete.
        let mut parsed: Option<(usize, usize)> = None;

        loop {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            let prev_len = data.len();
            data.extend_from_slice(&tmp[..n]);

            if data.len() > MAX_REQUEST_SIZE {
                break;
            }

            if parsed.is_none() {
                let from4 = prev_len.saturating_sub(3);
                let from2 = prev_len.saturating_sub(1);
                let header_end = data[from4..]
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| from4 + p + 4)
                    .or_else(|| {
                        data[from2..]
                            .windows(2)
                            .position(|w| w == b"\n\n")
                            .map(|p| from2 + p + 2)
                    });
                match header_end {
                    Some(body_offset) => {
                        let headers = String::from_utf8_lossy(&data[..body_offset]);
                        let content_length = headers
                            .lines()
                            .find(|l| l.to_lowercase().starts_with("content-length:"))
                            .and_then(|l| l.split_once(':'))
                            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        parsed = Some((body_offset, content_length));
                    }
                    None if data.len() > MAX_HEADER_SIZE => break,
                    None => {}
                }
            }

            if let Some((body_offset, content_length)) = parsed {
                if data.len() - body_offset >= content_length {
                    break; // We have the full request
                }
            }
        }
        Ok(data)
    }

    async fn handle_connection<S: AsyncReadExt + AsyncWriteExt + Unpin>(
        mut socket: S,
        handler: Arc<RpcHandler>,
        auth: &RpcAuthenticator,
        peer_ip: std::net::IpAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let data = match tokio::time::timeout(
            RPC_READ_TIMEOUT,
            Self::read_http_request(&mut socket),
        )
        .await
        {
            Ok(r) => r?,
            Err(_) => return Ok(()), // slow / idle client — drop
        };

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
                id: Value::Null,
                result: None,
                error: Some(RpcError {
                    code: -32700,
                    message: "No request body".to_string(),
                }),
            }
        } else {
            match serde_json::from_str::<RpcRequest>(body.trim_end_matches('\0')) {
                Ok(request) => {
                    // Only require auth for non-public (state-modifying) methods.
                    // If no credentials are configured, privileged methods are
                    // served to loopback callers only (fail closed, not open).
                    if !is_public_method(&request.method) {
                        let authorized = if !auth.is_enabled() {
                            peer_ip.is_loopback()
                        } else {
                            http_request
                                .lines()
                                .find(|line| {
                                    let lower = line.to_lowercase();
                                    lower.starts_with("authorization:")
                                })
                                .and_then(|line| {
                                    line.split_once(':').map(|(_, v)| v.trim().to_string())
                                })
                                .map(|value| auth.check(&value))
                                .unwrap_or(false)
                        };

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
                            let _ = tokio::time::timeout(RPC_WRITE_TIMEOUT, async {
                                socket.write_all(http_response.as_bytes()).await?;
                                socket.flush().await
                            })
                            .await;
                            return Ok(());
                        }
                    }

                    handler.handle_request(request).await
                }
                Err(e) => RpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: Value::Null,
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

        match tokio::time::timeout(RPC_WRITE_TIMEOUT, async {
            socket.write_all(http_response.as_bytes()).await?;
            socket.flush().await
        })
        .await
        {
            Ok(r) => r?,
            Err(_) => return Ok(()), // client not reading — drop
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn allow_entry_parsing_and_matching() {
        let (net, p) = parse_allow_entry("10.1.2.0/24").unwrap();
        assert!(ip_in_cidr(ip("10.1.2.99"), net, p));
        assert!(!ip_in_cidr(ip("10.1.3.1"), net, p));

        let (net, p) = parse_allow_entry("203.0.113.7").unwrap();
        assert_eq!(p, 32);
        assert!(ip_in_cidr(ip("203.0.113.7"), net, p));
        assert!(!ip_in_cidr(ip("203.0.113.8"), net, p));

        let (net, p) = parse_allow_entry("2001:db8::/32").unwrap();
        assert!(ip_in_cidr(ip("2001:db8:1::5"), net, p));
        assert!(!ip_in_cidr(ip("10.0.0.1"), net, p));

        assert!(parse_allow_entry("10.0.0.0/33").is_none());
        assert!(parse_allow_entry("not-an-ip").is_none());
    }

    #[test]
    fn ipv4_mapped_is_canonicalised() {
        assert_eq!(canonical_ip(ip("::ffff:1.2.3.4")), ip("1.2.3.4"));
        assert_eq!(canonical_ip(ip("::1")), ip("::1"));
    }

    #[test]
    fn sensitive_methods_are_not_public() {
        for m in [
            "getmessages",
            "listcontacts",
            "getwalletinfo",
            "acknowledgepaymentrequest",
            "sendtoaddress",
            "stop",
        ] {
            assert!(!is_public_method(m), "{} must require auth", m);
        }
    }

    #[tokio::test]
    async fn reads_fragmented_request_with_body() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"getblockcount","params":[]}"#;
        let req = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (mut client, mut server) = tokio::io::duplex(16);
        let bytes = req.clone().into_bytes();
        tokio::spawn(async move {
            // Write in small pieces so the terminator straddles reads.
            for chunk in bytes.chunks(5) {
                client.write_all(chunk).await.unwrap();
            }
        });
        let data = RpcServer::read_http_request(&mut server).await.unwrap();
        assert_eq!(data, req.into_bytes());
    }
}
