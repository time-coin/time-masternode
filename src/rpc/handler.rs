use super::server::{RpcError, RpcRequest, RpcResponse};
use crate::consensus::ConsensusEngine;
use crate::utxo_manager::UTXOStateManager;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::SystemTime;

pub struct RpcHandler {
    consensus: Arc<ConsensusEngine>,
    #[allow(dead_code)]
    utxo_manager: Arc<UTXOStateManager>,
    start_time: SystemTime,
}

impl RpcHandler {
    pub fn new(consensus: Arc<ConsensusEngine>, utxo_manager: Arc<UTXOStateManager>) -> Self {
        Self {
            consensus,
            utxo_manager,
            start_time: SystemTime::now(),
        }
    }
    pub async fn handle_request(&self, request: RpcRequest) -> RpcResponse {
        let result = match request.method.as_str() {
            "getblockchaininfo" => self.get_blockchain_info().await,
            "getblockcount" => self.get_block_count().await,
            "getnetworkinfo" => self.get_network_info().await,
            "getconsensusinfo" => self.get_consensus_info().await,
            "masternodelist" => self.masternode_list().await,
            "uptime" => self.uptime().await,
            _ => Err(RpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
            }),
        };

        match result {
            Ok(value) => RpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(value),
                error: None,
            },
            Err(error) => RpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(error),
            },
        }
    }

    async fn get_blockchain_info(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "chain": "main",
            "blocks": 1,
            "consensus": "BFT",
            "instant_finality": true
        }))
    }

    async fn get_block_count(&self) -> Result<Value, RpcError> {
        Ok(json!(1))
    }

    async fn get_network_info(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "version": 10000,
            "subversion": "/timed:0.1.0/",
            "connections": 0
        }))
    }

    async fn get_consensus_info(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "type": "BFT",
            "masternodes": self.consensus.masternodes.len(),
            "quorum": (2 * self.consensus.masternodes.len()).div_ceil(3)
        }))
    }

    async fn masternode_list(&self) -> Result<Value, RpcError> {
        let list: Vec<Value> = self
            .consensus
            .masternodes
            .iter()
            .map(|mn| {
                json!({
                    "address": mn.address,
                    "collateral": mn.collateral
                })
            })
            .collect();
        Ok(json!(list))
    }

    async fn uptime(&self) -> Result<Value, RpcError> {
        let uptime = SystemTime::now()
            .duration_since(self.start_time)
            .unwrap()
            .as_secs();
        Ok(json!(uptime))
    }
}
