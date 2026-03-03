//! Alternative RPC server implementation using raw TCP.
//!
//! Note: This module is currently unused - the application uses an axum-based
//! HTTP RPC server implemented directly in main.rs. This implementation is kept
//! as an alternative option for scenarios where a simpler TCP-based JSON-RPC
//! server might be preferred over HTTP.
//!
//! To use this instead of the axum server:
//! 1. Create RpcServer with dependencies
//! 2. Call server.run() instead of the axum router

#![allow(dead_code)]

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

pub struct RpcServer {
    listener: TcpListener,
    handler: Arc<RpcHandler>,
    /// Base64-encoded "user:password" for HTTP Basic Auth. Empty = no auth required.
    auth_token: String,
    rate_limiter: Arc<RpcRateLimiter>,
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

        let auth_token = if !rpcuser.is_empty() && !rpcpassword.is_empty() {
            base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", rpcuser, rpcpassword))
        } else {
            String::new()
        };

        Ok(Self {
            listener,
            handler: Arc::new(handler),
            auth_token,
            rate_limiter: Arc::new(RpcRateLimiter::new(100)),
        })
    }

    pub async fn run(&mut self) -> Result<(), std::io::Error> {
        println!(
            "  ✅ RPC server listening on {} (auth: {})",
            self.listener.local_addr()?,
            if self.auth_token.is_empty() {
                "disabled"
            } else {
                "enabled"
            }
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

            // Rate limit check
            if !self.rate_limiter.check(addr.ip()) {
                let _ =
                    Self::send_error(socket, "Rate limit exceeded. Try again later.", -32005).await;
                continue;
            }

            let handler = self.handler.clone();
            let auth_token = self.auth_token.clone();

            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(socket, handler, &auth_token).await {
                    eprintln!("RPC error: {}", e);
                }
            });
        }
    }

    /// Send a JSON-RPC error response and close the connection.
    async fn send_error(
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

    async fn handle_connection(
        mut socket: tokio::net::TcpStream,
        handler: Arc<RpcHandler>,
        auth_token: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = vec![0u8; 8192];
        let bytes_read = socket.read(&mut buffer).await?;

        if bytes_read == 0 {
            return Ok(());
        }

        let http_request = String::from_utf8_lossy(&buffer[..bytes_read]);

        // Check HTTP Basic Auth if credentials are configured
        if !auth_token.is_empty() {
            let authorized = http_request
                .lines()
                .find(|line| {
                    let lower = line.to_lowercase();
                    lower.starts_with("authorization:")
                })
                .and_then(|line| line.split_once(':').map(|(_, v)| v.trim().to_string()))
                .map(|value| {
                    if let Some(token) = value.strip_prefix("Basic ") {
                        token.trim() == auth_token
                    } else {
                        false
                    }
                })
                .unwrap_or(false);

            if !authorized {
                let error_response = RpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: "unauthorized".to_string(),
                    result: None,
                    error: Some(RpcError {
                        code: -32600,
                        message: "Unauthorized: invalid or missing RPC credentials".to_string(),
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
            // Parse and handle request
            match serde_json::from_str::<RpcRequest>(body.trim_end_matches('\0')) {
                Ok(request) => handler.handle_request(request).await,
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
