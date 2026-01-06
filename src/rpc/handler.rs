//! RPC handler for the alternative TCP-based RPC server.
//!
//! See server.rs for details on why this module is currently unused.

#![allow(dead_code)]

use super::server::{RpcError, RpcRequest, RpcResponse};
use crate::consensus::ConsensusEngine;
use crate::heartbeat_attestation::HeartbeatAttestationSystem;
use crate::masternode_registry::MasternodeRegistry;
use crate::types::{OutPoint, Transaction, TxInput, TxOutput};
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::time::Duration;

pub struct RpcHandler {
    consensus: Arc<ConsensusEngine>,
    utxo_manager: Arc<UTXOStateManager>,
    registry: Arc<MasternodeRegistry>,
    blockchain: Arc<crate::blockchain::Blockchain>,
    attestation_system: Arc<HeartbeatAttestationSystem>,
    blacklist: Arc<tokio::sync::RwLock<crate::network::blacklist::IPBlacklist>>,
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
        blockchain: Arc<crate::blockchain::Blockchain>,
        attestation_system: Arc<HeartbeatAttestationSystem>,
        blacklist: Arc<tokio::sync::RwLock<crate::network::blacklist::IPBlacklist>>,
    ) -> Self {
        Self {
            consensus,
            utxo_manager,
            registry,
            blockchain,
            attestation_system,
            blacklist,
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
            "getavalanchestatus" => self.get_avalanche_status().await,
            "validateaddress" => self.validate_address(&params_array).await,
            "stop" => self.stop().await,
            "uptime" => self.uptime().await,
            "getmempoolinfo" => self.get_mempool_info().await,
            "getrawmempool" => self.get_raw_mempool().await,
            "sendtoaddress" => self.send_to_address(&params_array).await,
            "mergeutxos" => self.merge_utxos(&params_array).await,
            "getattestationstats" => self.get_attestation_stats().await,
            "getheartbeathistory" => match params_array.first().and_then(|v| v.as_str()) {
                Some(address) => {
                    let limit = params_array.get(1).and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                    self.get_heartbeat_history(address, limit).await
                }
                None => Err(RpcError {
                    code: -32602,
                    message: "address parameter required".to_string(),
                }),
            },
            "gettransactionfinality" => self.get_transaction_finality(&params_array).await,
            "waittransactionfinality" => self.wait_transaction_finality(&params_array).await,
            "getwhitelist" => self.get_whitelist().await,
            "addwhitelist" => self.add_whitelist(&params_array).await,
            "removewhitelist" => self.remove_whitelist(&params_array).await,
            "getblacklist" => self.get_blacklist().await,
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
        let height = self.blockchain.get_height();
        let best_hash = self.blockchain.get_block_hash(height).unwrap_or([0u8; 32]);

        Ok(json!({
            "chain": chain,
            "blocks": height,
            "headers": height,
            "bestblockhash": hex::encode(best_hash),
            "difficulty": 1.0,
            "mediantime": chrono::Utc::now().timestamp(),
            "verificationprogress": 1.0,
            "chainwork": format!("{:064x}", height),
            "pruned": false,
            "consensus": "Avalanche + TSDC",
            "finality_mechanism": "Avalanche consensus",
            "instant_finality": true,
            "average_finality_time_ms": 750,
            "block_time_seconds": 600
        }))
    }

    async fn get_block_count(&self) -> Result<Value, RpcError> {
        let height = self.blockchain.get_height();
        Ok(json!(height))
    }

    async fn get_block(&self, params: &[Value]) -> Result<Value, RpcError> {
        let height = params
            .first()
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected block height".to_string(),
            })?;

        // Get block from blockchain
        match self.blockchain.get_block_by_height(height).await {
            Ok(block) => {
                let txids: Vec<String> = block
                    .transactions
                    .iter()
                    .map(|tx| hex::encode(tx.txid()))
                    .collect();

                // Calculate block hash
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(block.header.height.to_le_bytes());
                hasher.update(block.header.previous_hash);
                hasher.update(block.header.merkle_root);
                hasher.update(block.header.timestamp.to_le_bytes());
                let block_hash: [u8; 32] = hasher.finalize().into();

                Ok(json!({
                    "height": block.header.height,
                    "hash": hex::encode(block_hash),
                    "previousblockhash": hex::encode(block.header.previous_hash),
                    "time": block.header.timestamp,
                    "version": block.header.version,
                    "merkleroot": hex::encode(block.header.merkle_root),
                    "tx": txids,
                    "nTx": block.transactions.len(),
                    "confirmations": (self.blockchain.get_height() as i64 - height as i64 + 1).max(0),
                    "block_reward": block.header.block_reward,
                    "masternode_rewards": block.masternode_rewards.iter().map(|(addr, amount)| {
                        json!({
                            "address": addr,
                            "amount": amount
                        })
                    }).collect::<Vec<_>>(),
                }))
            }
            Err(e) => Err(RpcError {
                code: -5,
                message: format!("Block not found: {}", e),
            }),
        }
    }

    async fn get_network_info(&self) -> Result<Value, RpcError> {
        let network = match self.network {
            NetworkType::Mainnet => "mainnet",
            NetworkType::Testnet => "testnet",
        };

        // Get active peer count from registry (masternodes)
        let active_masternodes = self.registry.count_active().await;

        Ok(json!({
            "version": 100000,
            "subversion": "/timed:1.0.0/",
            "protocolversion": 70016,
            "localservices": "0000000000000409",
            "localrelay": true,
            "timeoffset": 0,
            "networkactive": true,
            "connections": active_masternodes,
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
        let masternodes = self.registry.list_all().await;
        let peers: Vec<Value> = masternodes
            .iter()
            .map(|mn| {
                json!({
                    "addr": mn.masternode.address.clone(),
                    "services": "0000000000000409",
                    "lastseen": mn.last_heartbeat,
                    "subver": "/timed:1.0.0/",
                    "inbound": false,
                    "conntime": mn.masternode.registered_at,
                    "timeoffset": 0,
                    "version": 100000,
                    "is_masternode": true,
                    "tier": format!("{:?}", mn.masternode.tier),
                    "active": mn.is_active,
                })
            })
            .collect();
        Ok(json!(peers))
    }

    async fn get_txout_set_info(&self) -> Result<Value, RpcError> {
        let utxos = self.utxo_manager.list_all_utxos().await;
        let total_amount: u64 = utxos.iter().map(|u| u.value).sum();
        let height = self.blockchain.get_height();

        Ok(json!({
            "height": height,
            "bestblock": hex::encode(self.blockchain.get_block_hash(height).unwrap_or([0u8; 32])),
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
        let hex_tx = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected transaction hex".to_string(),
            })?;

        // Decode hex transaction
        let tx_bytes = hex::decode(hex_tx).map_err(|_| RpcError {
            code: -22,
            message: "TX decode failed".to_string(),
        })?;

        // Deserialize transaction
        let tx: Transaction = bincode::deserialize(&tx_bytes).map_err(|_| RpcError {
            code: -22,
            message: "TX deserialization failed".to_string(),
        })?;

        let txid = tx.txid();

        // Validate transaction basic format
        if tx.inputs.is_empty() || tx.outputs.is_empty() {
            return Err(RpcError {
                code: -26,
                message: "TX missing inputs or outputs".to_string(),
            });
        }

        // Verify all outputs have valid amounts
        for output in &tx.outputs {
            if output.value == 0 {
                return Err(RpcError {
                    code: -26,
                    message: "TX output value cannot be zero".to_string(),
                });
            }
        }

        // Add to mempool
        {
            let mut mempool = self.mempool.write().await;
            mempool.insert(txid, tx.clone());
        }

        // Process transaction through consensus
        // Start Avalanche consensus to finalize this transaction
        tokio::spawn({
            let consensus = self.consensus.clone();
            let tx_for_consensus = tx.clone();
            async move {
                // Initiate Avalanche consensus for transaction
                if let Err(e) = consensus.add_transaction(tx_for_consensus).await {
                    tracing::error!("Failed to process transaction through consensus: {}", e);
                }
            }
        });

        Ok(json!(hex::encode(txid)))
    }

    async fn create_raw_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let inputs = params
            .first()
            .and_then(|v| v.as_array())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected inputs array".to_string(),
            })?;

        let outputs = params
            .get(1)
            .and_then(|v| v.as_object())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected outputs object".to_string(),
            })?;

        // Parse inputs into TxInputs
        let mut tx_inputs = Vec::new();
        for input in inputs {
            let input_obj = input.as_object().ok_or_else(|| RpcError {
                code: -8,
                message: "Invalid input format".to_string(),
            })?;

            let txid_str = input_obj
                .get("txid")
                .and_then(|v| v.as_str())
                .ok_or_else(|| RpcError {
                    code: -8,
                    message: "Missing txid in input".to_string(),
                })?;

            let vout = input_obj
                .get("vout")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| RpcError {
                    code: -8,
                    message: "Missing vout in input".to_string(),
                })? as u32;

            let txid_bytes = hex::decode(txid_str).map_err(|_| RpcError {
                code: -8,
                message: "Invalid txid hex format".to_string(),
            })?;

            if txid_bytes.len() != 32 {
                return Err(RpcError {
                    code: -8,
                    message: "Invalid txid length".to_string(),
                });
            }

            let mut txid_array = [0u8; 32];
            txid_array.copy_from_slice(&txid_bytes);

            tx_inputs.push(TxInput {
                previous_output: OutPoint {
                    txid: txid_array,
                    vout,
                },
                script_sig: vec![],
                sequence: 0xffffffff,
            });
        }

        // Parse outputs into TxOutputs
        let mut tx_outputs = Vec::new();
        for (address, amount_val) in outputs.iter() {
            let amount = amount_val.as_f64().ok_or_else(|| RpcError {
                code: -8,
                message: "Invalid amount value".to_string(),
            })? * 100_000_000.0; // Convert to satoshis

            if amount <= 0.0 || amount.is_nan() {
                return Err(RpcError {
                    code: -8,
                    message: "Invalid amount".to_string(),
                });
            }

            tx_outputs.push(TxOutput {
                value: amount as u64,
                script_pubkey: address.as_bytes().to_vec(),
            });
        }

        // Create transaction
        let tx = Transaction {
            version: 1,
            inputs: tx_inputs,
            outputs: tx_outputs,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            lock_time: 0,
        };

        // Serialize and return hex
        let tx_bytes = bincode::serialize(&tx).map_err(|_| RpcError {
            code: -32603,
            message: "Failed to serialize transaction".to_string(),
        })?;

        Ok(json!(hex::encode(tx_bytes)))
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
        if let Some(local_mn) = self.registry.get_local_masternode().await {
            Ok(json!({
                "status": "active",
                "address": local_mn.masternode.address,
                "reward_address": local_mn.reward_address,
                "tier": format!("{:?}", local_mn.masternode.tier),
                "total_uptime": local_mn.total_uptime,
                "is_active": local_mn.is_active,
                "public_key": hex::encode(local_mn.masternode.public_key.to_bytes())
            }))
        } else {
            Ok(json!({
                "status": "Not a masternode",
                "message": "This node is not configured as a masternode"
            }))
        }
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
        // Graceful shutdown via RPC
        //
        // Current implementation: Exits after 1 second delay
        // This works but doesn't allow graceful cleanup of:
        // - Open network connections
        // - Pending database writes
        // - In-flight RPC requests
        //
        // For full graceful shutdown, would need:
        // 1. Add shutdown_manager: Arc<ShutdownManager> to RpcHandler struct
        // 2. Call shutdown_manager.initiate_shutdown().await here
        // 3. ShutdownManager coordinates cleanup across all subsystems
        //
        // For now, this simple exit is acceptable for RPC shutdown requests
        tracing::info!("🛑 Shutdown requested via RPC, exiting in 1 second...");
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
        let masternodes = self.consensus.get_active_masternodes();
        let mn_count = masternodes.len();

        // Avalanche consensus parameters
        let avalanche_config = json!({
            "protocol": "Avalanche + TSDC",
            "avalanche": {
                "sample_size": 20,
                "finality_confidence": 15,
                "query_timeout_ms": 2000,
                "description": "Instant transaction finality via random validator sampling"
            },
            "tsdc": {
                "block_time_seconds": 600,
                "leader_selection": "Verifiable Random Function (VRF)",
                "description": "Deterministic 10-minute block production"
            },
            "active_validators": mn_count,
            "finality_type": "Avalanche consensus (seconds) + TSDC blocks (10 minutes)",
            "instant_finality": true,
            "average_finality_time_ms": 750
        });

        Ok(avalanche_config)
    }

    /// Get Avalanche consensus status and metrics
    async fn get_avalanche_status(&self) -> Result<Value, RpcError> {
        let masternodes = self.consensus.get_active_masternodes();
        let active_validators = masternodes.len();

        Ok(json!({
            "protocol": "Avalanche",
            "status": "active",
            "active_validators": active_validators,
            "configuration": {
                "sample_size": 20,
                "finality_threshold": 15,
                "query_timeout_ms": 2000,
                "max_rounds": 100
            },
            "metrics": {
                "average_finality_time_ms": 750,
                "finality_type": "probabilistic (cryptographically secure)",
                "validator_sampling": "random k-of-n",
                "description": "Avalanche consensus: query random 20 validators per round, finalize after 15 consecutive confirms"
            },
            "note": "Transactions finalized by Avalanche in seconds, blocks produced every 10 minutes by TSDC"
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

    async fn send_to_address(&self, params: &[Value]) -> Result<Value, RpcError> {
        // Parse parameters: sendtoaddress "address" amount
        let to_address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected address".to_string(),
            })?;

        let amount = params
            .get(1)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected amount".to_string(),
            })?;

        // Convert TIME to smallest unit (like satoshis)
        let amount_units = (amount * 100_000_000.0) as u64;

        // Get UTXOs for this wallet
        let utxos = self.utxo_manager.list_all_utxos().await;

        // Find sufficient UTXOs
        let mut selected_utxos = Vec::new();
        let mut total_input = 0u64;
        let fee = 1_000; // 0.00001 TIME fee

        for utxo in utxos {
            selected_utxos.push(utxo.clone());
            total_input += utxo.value;
            if total_input >= amount_units + fee {
                break;
            }
        }

        if total_input < amount_units + fee {
            return Err(RpcError {
                code: -6,
                message: "Insufficient funds".to_string(),
            });
        }

        // Create transaction
        use crate::types::{Transaction, TxInput, TxOutput};

        let inputs: Vec<TxInput> = selected_utxos
            .iter()
            .map(|utxo| TxInput {
                previous_output: utxo.outpoint.clone(),
                script_sig: vec![], // TODO: Sign with wallet key
                sequence: 0xFFFFFFFF,
            })
            .collect();

        let mut outputs = vec![TxOutput {
            value: amount_units,
            script_pubkey: to_address.as_bytes().to_vec(),
        }];

        // Add change output if necessary
        let change = total_input - amount_units - fee;
        if change > 0 {
            outputs.push(TxOutput {
                value: change,
                script_pubkey: vec![], // TODO: Get wallet's own address
            });
        }

        let tx = Transaction {
            version: 1,
            inputs,
            outputs,
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
        };

        let txid = tx.txid();

        // Broadcast transaction to consensus engine
        match self.consensus.process_transaction(tx).await {
            Ok(_) => Ok(json!(hex::encode(txid))),
            Err(e) => Err(RpcError {
                code: -26,
                message: format!("Transaction rejected: {}", e),
            }),
        }
    }

    async fn merge_utxos(&self, params: &[Value]) -> Result<Value, RpcError> {
        // Parse parameters: mergeutxos min_count max_count [address]
        let min_count = params.first().and_then(|v| v.as_u64()).unwrap_or(2) as usize;

        let max_count = params.get(1).and_then(|v| v.as_u64()).unwrap_or(100) as usize;

        let filter_address = params.get(2).and_then(|v| v.as_str());

        // Get all UTXOs
        let mut utxos = self.utxo_manager.list_all_utxos().await;

        // Filter by address if specified
        if let Some(addr) = filter_address {
            utxos.retain(|utxo| utxo.address == addr);
        }

        // Check if we have enough UTXOs to merge
        if utxos.len() < min_count {
            return Err(RpcError {
                code: -8,
                message: format!(
                    "Not enough UTXOs to merge. Found {}, need at least {}",
                    utxos.len(),
                    min_count
                ),
            });
        }

        // Limit to max_count UTXOs
        utxos.truncate(max_count);

        tracing::info!("Merging {} UTXOs", utxos.len());

        // Calculate total value
        let total_value: u64 = utxos.iter().map(|u| u.value).sum();
        let fee = 1_000 + (utxos.len() as u64 * 100); // Base fee + per-input fee

        if total_value <= fee {
            return Err(RpcError {
                code: -8,
                message: format!(
                    "Total UTXO value ({}) is less than or equal to fee ({})",
                    total_value, fee
                ),
            });
        }

        // Create merge transaction
        use crate::types::{Transaction, TxInput, TxOutput};

        let inputs: Vec<TxInput> = utxos
            .iter()
            .map(|utxo| TxInput {
                previous_output: utxo.outpoint.clone(),
                script_sig: vec![], // TODO: Sign with wallet key
                sequence: 0xFFFFFFFF,
            })
            .collect();

        // Get the address from the first UTXO (all should be same if filtered)
        let output_address = if utxos.is_empty() {
            return Err(RpcError {
                code: -8,
                message: "No UTXOs selected".to_string(),
            });
        } else {
            &utxos[0].address
        };

        let outputs = vec![TxOutput {
            value: total_value - fee,
            script_pubkey: output_address.as_bytes().to_vec(),
        }];

        let tx = Transaction {
            version: 1,
            inputs,
            outputs,
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
        };

        let txid = tx.txid();

        // Broadcast transaction to consensus engine
        match self.consensus.process_transaction(tx).await {
            Ok(_) => Ok(json!({
                "txid": hex::encode(txid),
                "merged_count": utxos.len(),
                "total_value": total_value,
                "fee": fee,
                "final_value": total_value - fee,
                "message": format!("Successfully merged {} UTXOs", utxos.len())
            })),
            Err(e) => Err(RpcError {
                code: -26,
                message: format!("Transaction rejected: {}", e),
            }),
        }
    }

    async fn get_attestation_stats(&self) -> Result<Value, RpcError> {
        let stats = self.attestation_system.get_stats().await;

        Ok(json!({
            "total_heartbeats": stats.total_heartbeats,
            "verified_heartbeats": stats.verified_heartbeats,
            "pending_heartbeats": stats.pending_heartbeats,
            "unique_masternodes": stats.unique_masternodes,
            "total_verified_count": stats.total_verified_count,
            "verification_rate": if stats.total_heartbeats > 0 {
                (stats.verified_heartbeats as f64 / stats.total_heartbeats as f64) * 100.0
            } else {
                0.0
            }
        }))
    }

    async fn get_heartbeat_history(&self, address: &str, limit: usize) -> Result<Value, RpcError> {
        let history = self
            .attestation_system
            .get_heartbeat_history(address, limit)
            .await;
        let verified_count = self
            .attestation_system
            .get_verified_heartbeats(address)
            .await;
        let latest_seq = self.attestation_system.get_latest_sequence(address).await;

        let heartbeats: Vec<Value> = history.iter().map(|h| {
            json!({
                "sequence": h.heartbeat.sequence_number,
                "timestamp": h.heartbeat.timestamp,
                "verified": h.is_verified(),
                "witness_count": h.attestations.len(),
                "unique_witnesses": h.unique_witnesses(),
                "witnesses": h.attestations.iter().map(|a| &a.witness_address).collect::<Vec<_>>()
            })
        }).collect();

        Ok(json!({
            "address": address,
            "total_verified_heartbeats": verified_count,
            "latest_sequence": latest_seq,
            "recent_heartbeats": heartbeats
        }))
    }

    async fn get_transaction_finality(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Transaction ID parameter required".to_string(),
            })?;

        let txid_bytes = hex::decode(txid).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid transaction ID format".to_string(),
        })?;

        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "Transaction ID must be 32 bytes".to_string(),
            });
        }

        let mut txid_array = [0u8; 32];
        txid_array.copy_from_slice(&txid_bytes);

        // Check if transaction is finalized
        if self.blockchain.is_transaction_finalized(&txid_array).await {
            let confirmations = self
                .blockchain
                .get_transaction_confirmations(&txid_array)
                .await
                .unwrap_or(0);
            return Ok(json!({
                "txid": txid,
                "finalized": true,
                "confirmations": confirmations,
                "finality_type": "avalanche"
            }));
        }

        // Check if transaction is in mempool
        let mempool = self.mempool.read().await;
        if mempool.contains_key(&txid_array) {
            return Ok(json!({
                "txid": txid,
                "finalized": false,
                "status": "pending",
                "in_mempool": true
            }));
        }
        drop(mempool);

        // Transaction not found
        Err(RpcError {
            code: -5,
            message: format!("Transaction not found: {}", txid),
        })
    }

    async fn wait_transaction_finality(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Transaction ID parameter required".to_string(),
            })?;

        let timeout_secs = params.get(1).and_then(|v| v.as_u64()).unwrap_or(300);

        let txid_bytes = hex::decode(txid).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid transaction ID format".to_string(),
        })?;

        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "Transaction ID must be 32 bytes".to_string(),
            });
        }

        let mut txid_array = [0u8; 32];
        txid_array.copy_from_slice(&txid_bytes);

        let start_time = tokio::time::Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            // Check if transaction is finalized
            if self.blockchain.is_transaction_finalized(&txid_array).await {
                let confirmations = self
                    .blockchain
                    .get_transaction_confirmations(&txid_array)
                    .await
                    .unwrap_or(0);
                return Ok(json!({
                    "txid": txid,
                    "finalized": true,
                    "confirmations": confirmations,
                    "finality_type": "avalanche",
                    "wait_time_ms": start_time.elapsed().as_millis()
                }));
            }

            // Check timeout
            if start_time.elapsed() >= timeout {
                return Err(RpcError {
                    code: -11,
                    message: format!(
                        "Transaction finality timeout after {} seconds",
                        timeout_secs
                    ),
                });
            }

            // Wait a bit before checking again
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Get whitelist info (list of whitelisted IPs)
    async fn get_whitelist(&self) -> Result<Value, RpcError> {
        let bl = self.blacklist.read().await;
        let (_, _, _, whitelist_count) = bl.stats();

        Ok(json!({
            "count": whitelist_count,
            "info": "Whitelisted IPs are exempt from rate limiting and bans. Use 'addwhitelist <ip>' to add."
        }))
    }

    /// Add IP to whitelist
    async fn add_whitelist(&self, params: &[Value]) -> Result<Value, RpcError> {
        let ip_str = params.first().and_then(|v| v.as_str()).ok_or(RpcError {
            code: -32602,
            message: "IP address parameter required".to_string(),
        })?;

        let ip_addr = ip_str.parse::<std::net::IpAddr>().map_err(|_| RpcError {
            code: -32602,
            message: format!("Invalid IP address: {}", ip_str),
        })?;

        let mut bl = self.blacklist.write().await;
        let was_whitelisted = bl.is_whitelisted(ip_addr);

        if was_whitelisted {
            Ok(json!({
                "result": "already_whitelisted",
                "ip": ip_str,
                "message": "IP is already whitelisted"
            }))
        } else {
            bl.add_to_whitelist(ip_addr, "Added via RPC");
            Ok(json!({
                "result": "success",
                "ip": ip_str,
                "message": "IP added to whitelist"
            }))
        }
    }

    /// Remove IP from whitelist
    async fn remove_whitelist(&self, params: &[Value]) -> Result<Value, RpcError> {
        let ip_str = params.first().and_then(|v| v.as_str()).ok_or(RpcError {
            code: -32602,
            message: "IP address parameter required".to_string(),
        })?;

        let _ip_addr = ip_str.parse::<std::net::IpAddr>().map_err(|_| RpcError {
            code: -32602,
            message: format!("Invalid IP address: {}", ip_str),
        })?;

        // Note: We don't implement removal to prevent accidental removal of masternodes
        // Whitelisting is permanent by design
        Ok(json!({
            "result": "not_supported",
            "message": "Whitelist removal not supported. Whitelisting is permanent by design to protect masternode connections."
        }))
    }

    /// Get blacklist statistics
    async fn get_blacklist(&self) -> Result<Value, RpcError> {
        let bl = self.blacklist.read().await;
        let (permanent, temporary, violations, whitelist) = bl.stats();

        Ok(json!({
            "permanent_bans": permanent,
            "temporary_bans": temporary,
            "active_violations": violations,
            "whitelisted": whitelist
        }))
    }
}
