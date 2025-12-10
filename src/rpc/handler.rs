use super::server::{RpcError, RpcRequest, RpcResponse};
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::types::Transaction;
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

pub struct RpcHandler {
    consensus: Arc<ConsensusEngine>,
    utxo_manager: Arc<UTXOStateManager>,
    registry: Arc<MasternodeRegistry>,
    start_time: SystemTime,
    network: NetworkType,
    mempool: Arc<tokio::sync::RwLock<HashMap<[u8; 32], Transaction>>>,
}

impl RpcHandler {
    pub fn new(
        consensus: Arc<ConsensusEngine>,
        utxo_manager: Arc<UTXOStateManager>,
        network: NetworkType,
        registry: Arc<MasternodeRegistry>,
    ) -> Self {
        Self {
            consensus,
            utxo_manager,
            registry,
            start_time: SystemTime::now(),
            network,
            mempool: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }
    pub async fn handle_request(&self, request: RpcRequest) -> RpcResponse {
        // Convert params Value to array
        let params_array = match &request.params {
            Value::Array(arr) => arr.clone(),
            Value::Null => vec![],
            other => vec![other.clone()],
        };

        let result = match request.method.as_str() {
            "getblockchaininfo" => self.get_blockchain_info().await,
            "getblockcount" => self.get_block_count().await,
            "getblock" => self.get_block(&params_array).await,
            "getnetworkinfo" => self.get_network_info().await,
            "getpeerinfo" => self.get_peer_info().await,
            "gettxoutsetinfo" => self.get_txout_set_info().await,
            "getrawtransaction" => self.get_raw_transaction(&params_array).await,
            "gettransaction" => self.get_transaction(&params_array).await,
            "sendrawtransaction" => self.send_raw_transaction(&params_array).await,
            "createrawtransaction" => self.create_raw_transaction(&params_array).await,
            "getbalance" => self.get_balance(&params_array).await,
            "listunspent" => self.list_unspent(&params_array).await,
            "masternodelist" => self.masternode_list().await,
            "masternodestatus" => self.masternode_status().await,
            "getconsensusinfo" => self.get_consensus_info().await,
            "validateaddress" => self.validate_address(&params_array).await,
            "stop" => self.stop().await,
            "uptime" => self.uptime().await,
            "getmempoolinfo" => self.get_mempool_info().await,
            "getrawmempool" => self.get_raw_mempool().await,
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
        let chain = match self.network {
            NetworkType::Mainnet => "main",
            NetworkType::Testnet => "test",
        };
        Ok(json!({
            "chain": chain,
            "blocks": 1,
            "headers": 1,
            "bestblockhash": format!("{:064x}", 1),
            "difficulty": 1.0,
            "mediantime": chrono::Utc::now().timestamp(),
            "verificationprogress": 1.0,
            "chainwork": "0000000000000000000000000000000000000000000000000000000000000001",
            "pruned": false,
            "consensus": "BFT",
            "instant_finality": true,
            "finality_time": "<3 seconds"
        }))
    }

    async fn get_block_count(&self) -> Result<Value, RpcError> {
        Ok(json!(1))
    }

    async fn get_block(&self, params: &[Value]) -> Result<Value, RpcError> {
        let height = params
            .first()
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected block height".to_string(),
            })?;

        // For now, return a stub block
        // TODO: Implement actual block storage and retrieval
        Ok(json!({
            "height": height,
            "hash": format!("{:064x}", height),
            "previousblockhash": format!("{:064x}", height.saturating_sub(1)),
            "time": chrono::Utc::now().timestamp(),
            "tx": [],
            "confirmations": 1
        }))
    }

    async fn get_network_info(&self) -> Result<Value, RpcError> {
        let network = match self.network {
            NetworkType::Mainnet => "mainnet",
            NetworkType::Testnet => "testnet",
        };
        Ok(json!({
            "version": 10000,
            "subversion": "/timed:0.1.0/",
            "protocolversion": 70016,
            "localservices": "0000000000000409",
            "localrelay": true,
            "timeoffset": 0,
            "networkactive": true,
            "connections": 0,
            "networks": [{
                "name": network,
                "limited": false,
                "reachable": true,
                "proxy": "",
                "proxy_randomize_credentials": false
            }],
            "relayfee": 0.00001,
            "incrementalfee": 0.00001,
            "localaddresses": [],
            "warnings": ""
        }))
    }

    async fn get_peer_info(&self) -> Result<Value, RpcError> {
        // TODO: Implement actual peer tracking
        Ok(json!([]))
    }

    async fn get_txout_set_info(&self) -> Result<Value, RpcError> {
        let utxos = self.utxo_manager.list_all_utxos().await;
        let total_amount: u64 = utxos.iter().map(|u| u.value).sum();

        Ok(json!({
            "height": 1,
            "bestblock": format!("{:064x}", 1),
            "transactions": utxos.len(),
            "txouts": utxos.len(),
            "total_amount": total_amount as f64 / 100_000_000.0,
            "disk_size": 0
        }))
    }

    async fn get_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected txid".to_string(),
            })?;

        let txid = hex::decode(txid_str).map_err(|_| RpcError {
            code: -8,
            message: "Invalid txid format".to_string(),
        })?;

        if txid.len() != 32 {
            return Err(RpcError {
                code: -8,
                message: "Invalid txid length".to_string(),
            });
        }

        // Check mempool first
        let mut txid_array = [0u8; 32];
        txid_array.copy_from_slice(&txid);

        if let Some(tx) = self.mempool.read().await.get(&txid_array) {
            return Ok(json!({
                "txid": hex::encode(txid_array),
                "version": tx.version,
                "size": 250, // Estimate
                "locktime": tx.lock_time,
                "vin": tx.inputs.iter().map(|input| json!({
                    "txid": hex::encode(input.previous_output.txid),
                    "vout": input.previous_output.vout,
                    "sequence": input.sequence
                })).collect::<Vec<_>>(),
                "vout": tx.outputs.iter().enumerate().map(|(i, output)| json!({
                    "value": output.value as f64 / 100_000_000.0,
                    "n": i,
                    "scriptPubKey": {
                        "hex": hex::encode(&output.script_pubkey)
                    }
                })).collect::<Vec<_>>(),
                "confirmations": 0,
                "time": tx.timestamp,
                "blocktime": tx.timestamp
            }));
        }

        Err(RpcError {
            code: -5,
            message: "No information available about transaction".to_string(),
        })
    }

    async fn get_raw_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let _txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected txid".to_string(),
            })?;

        let verbose = params.get(1).and_then(|v| v.as_bool()).unwrap_or(false);

        if verbose {
            self.get_transaction(params).await
        } else {
            Ok(json!("raw_transaction_hex_placeholder"))
        }
    }

    async fn send_raw_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let _hex_tx = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected transaction hex".to_string(),
            })?;

        // TODO: Decode and process transaction
        Ok(json!("txid_placeholder"))
    }

    async fn create_raw_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let _inputs = params
            .first()
            .and_then(|v| v.as_array())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected inputs array".to_string(),
            })?;

        let _outputs = params
            .get(1)
            .and_then(|v| v.as_object())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected outputs object".to_string(),
            })?;

        // TODO: Create transaction
        Ok(json!("raw_transaction_hex_placeholder"))
    }

    async fn get_balance(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params.first().and_then(|v| v.as_str());

        if let Some(addr) = address {
            // Get balance for specific address
            let utxos = self.utxo_manager.list_all_utxos().await;
            let balance: u64 = utxos
                .iter()
                .filter(|u| u.address == addr)
                .map(|u| u.value)
                .sum();

            Ok(json!(balance as f64 / 100_000_000.0))
        } else {
            // Get total wallet balance
            let utxos = self.utxo_manager.list_all_utxos().await;
            let balance: u64 = utxos.iter().map(|u| u.value).sum();
            Ok(json!(balance as f64 / 100_000_000.0))
        }
    }

    async fn list_unspent(&self, params: &[Value]) -> Result<Value, RpcError> {
        let _min_conf = params.first().and_then(|v| v.as_u64()).unwrap_or(1);
        let _max_conf = params.get(1).and_then(|v| v.as_u64()).unwrap_or(9999999);
        let addresses = params.get(2).and_then(|v| v.as_array());

        let utxos = self.utxo_manager.list_all_utxos().await;

        let filtered: Vec<Value> = utxos
            .iter()
            .filter(|u| {
                if let Some(addrs) = addresses {
                    addrs.iter().any(|a| a.as_str() == Some(&u.address))
                } else {
                    true
                }
            })
            .map(|u| {
                json!({
                    "txid": hex::encode(u.outpoint.txid),
                    "vout": u.outpoint.vout,
                    "address": u.address,
                    "amount": u.value as f64 / 100_000_000.0,
                    "confirmations": 1,
                    "spendable": true,
                    "solvable": true,
                    "safe": true
                })
            })
            .collect();

        Ok(json!(filtered))
    }

    async fn masternode_status(&self) -> Result<Value, RpcError> {
        // TODO: Return actual masternode status if running as masternode
        Ok(json!({
            "status": "Not a masternode",
            "message": "This node is not configured as a masternode"
        }))
    }

    async fn validate_address(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected address".to_string(),
            })?;

        let expected_prefix = match self.network {
            NetworkType::Mainnet => "TIME1",
            NetworkType::Testnet => "TIME0",
        };

        let is_valid = address.starts_with(expected_prefix) && address.len() > 10;

        Ok(json!({
            "isvalid": is_valid,
            "address": address,
            "scriptPubKey": if is_valid { hex::encode(address.as_bytes()) } else { String::new() },
            "ismine": false,
            "iswatchonly": false,
            "isscript": false,
            "iswitness": false
        }))
    }

    async fn stop(&self) -> Result<Value, RpcError> {
        // TODO: Implement graceful shutdown
        tokio::spawn(async {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            std::process::exit(0);
        });
        Ok(json!("TIME Coin server stopping"))
    }

    async fn get_mempool_info(&self) -> Result<Value, RpcError> {
        let mempool = self.mempool.read().await;
        let size = mempool.len();
        let bytes: usize = mempool.values().map(|_| 250).sum(); // Estimate

        Ok(json!({
            "loaded": true,
            "size": size,
            "bytes": bytes,
            "usage": bytes,
            "maxmempool": 300000000,
            "mempoolminfee": 0.00001,
            "minrelaytxfee": 0.00001
        }))
    }

    async fn get_raw_mempool(&self) -> Result<Value, RpcError> {
        let mempool = self.mempool.read().await;
        let txids: Vec<String> = mempool.keys().map(hex::encode).collect();
        Ok(json!(txids))
    }

    async fn get_consensus_info(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "type": "BFT",
            "masternodes": self.consensus.masternodes.len(),
            "quorum": (2 * self.consensus.masternodes.len()).div_ceil(3)
        }))
    }

    async fn masternode_list(&self) -> Result<Value, RpcError> {
        let masternodes = self.registry.list_all().await;
        let list: Vec<Value> = masternodes
            .iter()
            .map(|mn| {
                json!({
                    "address": mn.masternode.address,
                    "wallet_address": mn.masternode.wallet_address,
                    "collateral": mn.masternode.collateral,
                    "tier": format!("{:?}", mn.masternode.tier),
                    "registered_at": mn.masternode.registered_at,
                    "is_active": mn.is_active,
                    "last_heartbeat": mn.last_heartbeat,
                    "uptime_start": mn.uptime_start,
                    "total_uptime": mn.total_uptime,
                })
            })
            .collect();
        Ok(json!({
            "total": masternodes.len(),
            "masternodes": list
        }))
    }

    async fn uptime(&self) -> Result<Value, RpcError> {
        let uptime = SystemTime::now()
            .duration_since(self.start_time)
            .unwrap()
            .as_secs();
        Ok(json!(uptime))
    }
}
