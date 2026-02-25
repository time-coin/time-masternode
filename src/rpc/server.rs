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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
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

pub struct RpcServer {
    listener: TcpListener,
    handler: Arc<RpcHandler>,
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

        Ok(Self {
            listener,
            handler: Arc::new(handler),
        })
    }

    pub async fn run(&mut self) -> Result<(), std::io::Error> {
        println!(
            "  ✅ RPC server listening on {}",
            self.listener.local_addr()?
        );

        loop {
            let (socket, _addr) = self.listener.accept().await?;
            let handler = self.handler.clone();

            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(socket, handler).await {
                    eprintln!("RPC error: {}", e);
                }
            });
        }
    }

    async fn handle_connection(
        mut socket: tokio::net::TcpStream,
        handler: Arc<RpcHandler>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut buffer = vec![0u8; 8192];
        let bytes_read = socket.read(&mut buffer).await?;

        if bytes_read == 0 {
            return Ok(());
        }

        let http_request = String::from_utf8_lossy(&buffer[..bytes_read]);

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
