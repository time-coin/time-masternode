//! RPC handler for the alternative TCP-based RPC server.
//!
//! See server.rs for details on why this module is currently unused.

#![allow(dead_code)]

use super::server::{RpcError, RpcRequest, RpcResponse};
use crate::consensus::ConsensusEngine;
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
        blacklist: Arc<tokio::sync::RwLock<crate::network::blacklist::IPBlacklist>>,
    ) -> Self {
        Self {
            consensus,
            utxo_manager,
            registry,
            blockchain,
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
            "getbestblockhash" => self.get_best_block_hash().await,
            "getblockhash" => self.get_block_hash(&params_array).await,
            "getnetworkinfo" => self.get_network_info().await,
            "getpeerinfo" => self.get_peer_info().await,
            "gettxoutsetinfo" => self.get_txout_set_info().await,
            "getrawtransaction" => self.get_raw_transaction(&params_array).await,
            "gettransaction" => self.get_transaction(&params_array).await,
            "sendrawtransaction" => self.send_raw_transaction(&params_array).await,
            "createrawtransaction" => self.create_raw_transaction(&params_array).await,
            "decoderawtransaction" => self.decode_raw_transaction(&params_array).await,
            "getbalance" => self.get_balance(&params_array).await,
            "listunspent" => self.list_unspent(&params_array).await,
            "getnewaddress" => self.get_new_address(&params_array).await,
            "getwalletinfo" => self.get_wallet_info().await,
            "masternodelist" => self.masternode_list().await,
            "masternodestatus" => self.masternode_status().await,
            "masternoderegister" => self.masternode_register(&params_array).await,
            "masternodeunlock" => self.masternode_unlock(&params_array).await,
            "listlockedcollaterals" => self.list_locked_collaterals().await,
            "getconsensusinfo" => self.get_consensus_info().await,
            "gettimevotestatus" => self.get_timevote_status().await,
            "validateaddress" => self.validate_address(&params_array).await,
            "stop" => self.stop().await,
            "uptime" => self.uptime().await,
            "getmempoolinfo" => self.get_mempool_info().await,
            "getrawmempool" => self.get_raw_mempool().await,
            "sendtoaddress" => self.send_to_address(&params_array).await,
            "mergeutxos" => self.merge_utxos(&params_array).await,
            "gettransactionfinality" => self.get_transaction_finality(&params_array).await,
            "waittransactionfinality" => self.wait_transaction_finality(&params_array).await,
            "getwhitelist" => self.get_whitelist().await,
            "addwhitelist" => self.add_whitelist(&params_array).await,
            "removewhitelist" => self.remove_whitelist(&params_array).await,
            "getblacklist" => self.get_blacklist().await,
            "listreceivedbyaddress" => self.list_received_by_address(&params_array).await,
            "reindextransactions" => self.reindex_transactions().await,
            "gettxindexstatus" => self.get_tx_index_status().await,
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

        // Get real average finality time from consensus engine
        let avg_finality_ms = self.consensus.get_avg_finality_time_ms();

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
            "consensus": "timevote + TSDC",
            "finality_mechanism": "timevote consensus",
            "instant_finality": true,
            "average_finality_time_ms": avg_finality_ms,
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
                // Simulated ping time based on activity
                // TODO: Replace with actual ping times from peer connection registry
                let pingtime = if mn.is_active {
                    Some(0.020 + (rand::random::<f64>() * 0.030)) // 20-50ms for active nodes
                } else {
                    None
                };

                json!({
                    "addr": mn.masternode.address.clone(),
                    "services": "0000000000000409",
                    "lastseen": mn.masternode.registered_at,
                    "subver": "/timed:1.0.0/",
                    "inbound": false,
                    "conntime": mn.masternode.registered_at,
                    "timeoffset": 0,
                    "pingtime": pingtime,
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
                        "hex": hex::encode(&output.script_pubkey),
                        "address": String::from_utf8_lossy(&output.script_pubkey).to_string()
                    }
                })).collect::<Vec<_>>(),
                "confirmations": 0,
                "time": tx.timestamp,
                "blocktime": tx.timestamp
            }));
        }

        // Use transaction index for O(1) lookup if available
        if let Some(ref tx_index) = self.blockchain.tx_index {
            if let Some(location) = tx_index.get_location(&txid_array) {
                // Found in index - direct lookup
                if let Ok(block) = self
                    .blockchain
                    .get_block_by_height(location.block_height)
                    .await
                {
                    if let Some(tx) = block.transactions.get(location.tx_index) {
                        let current_height = self.blockchain.get_height();
                        let confirmations = current_height - location.block_height + 1;

                        return Ok(json!({
                            "txid": hex::encode(txid_array),
                            "version": tx.version,
                            "size": bincode::serialize(tx).map(|v| v.len()).unwrap_or(250),
                            "locktime": tx.lock_time,
                            "vin": tx.inputs.iter().map(|input| json!({
                                "txid": hex::encode(input.previous_output.txid),
                                "vout": input.previous_output.vout,
                                "sequence": input.sequence,
                                "scriptSig": {
                                    "hex": hex::encode(&input.script_sig)
                                }
                            })).collect::<Vec<_>>(),
                            "vout": tx.outputs.iter().enumerate().map(|(i, output)| json!({
                                "value": output.value as f64 / 100_000_000.0,
                                "n": i,
                                "scriptPubKey": {
                                    "hex": hex::encode(&output.script_pubkey),
                                    "address": String::from_utf8_lossy(&output.script_pubkey).to_string()
                                }
                            })).collect::<Vec<_>>(),
                            "confirmations": confirmations,
                            "time": tx.timestamp,
                            "blocktime": block.header.timestamp,
                            "blockhash": hex::encode(block.hash()),
                            "height": location.block_height
                        }));
                    }
                }
            }
        }

        // Fallback: Search blockchain for the transaction
        let current_height = self.blockchain.get_height();

        tracing::debug!(
            "Searching blockchain for transaction {} (height: 0-{})",
            hex::encode(txid_array),
            current_height
        );

        let mut blocks_searched = 0;
        let mut blocks_failed = 0;

        // Search entire blockchain from newest to oldest
        for height in (0..=current_height).rev() {
            match self.blockchain.get_block_by_height(height).await {
                Ok(block) => {
                    blocks_searched += 1;
                    for tx in &block.transactions {
                        if tx.txid() == txid_array {
                            tracing::info!(
                                "Found transaction {} in block {} (searched {} blocks)",
                                hex::encode(txid_array),
                                height,
                                blocks_searched
                            );
                            let confirmations = current_height - height + 1;
                            return Ok(json!({
                                "txid": hex::encode(txid_array),
                                "version": tx.version,
                                "size": bincode::serialize(tx).map(|v| v.len()).unwrap_or(250),
                                "locktime": tx.lock_time,
                                "vin": tx.inputs.iter().map(|input| json!({
                                    "txid": hex::encode(input.previous_output.txid),
                                    "vout": input.previous_output.vout,
                                    "sequence": input.sequence,
                                    "scriptSig": {
                                        "hex": hex::encode(&input.script_sig)
                                    }
                                })).collect::<Vec<_>>(),
                                "vout": tx.outputs.iter().enumerate().map(|(i, output)| json!({
                                    "value": output.value as f64 / 100_000_000.0,
                                    "n": i,
                                    "scriptPubKey": {
                                        "hex": hex::encode(&output.script_pubkey),
                                        "address": String::from_utf8_lossy(&output.script_pubkey).to_string()
                                    }
                                })).collect::<Vec<_>>(),
                                "confirmations": confirmations,
                                "time": tx.timestamp,
                                "blocktime": block.header.timestamp,
                                "blockhash": hex::encode(block.hash()),
                                "height": height
                            }));
                        }
                    }
                }
                Err(e) => {
                    blocks_failed += 1;
                    if blocks_failed < 5 {
                        // Only log first few failures
                        tracing::warn!("Failed to get block {} during tx search: {}", height, e);
                    }
                }
            }
        }

        tracing::warn!(
            "Transaction {} not found after searching {} blocks ({} failed)",
            hex::encode(txid_array),
            blocks_searched,
            blocks_failed
        );

        Err(RpcError {
            code: -5,
            message: format!(
                "No information available about transaction (searched {} blocks, {} failed)",
                blocks_searched, blocks_failed
            ),
        })
    }

    async fn get_raw_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected txid".to_string(),
            })?;

        let verbose = params.get(1).and_then(|v| v.as_bool()).unwrap_or(false);

        if verbose {
            // Return verbose JSON format
            self.get_transaction(params).await
        } else {
            // Return raw hex-encoded transaction
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

            let mut txid_array = [0u8; 32];
            txid_array.copy_from_slice(&txid);

            // Check mempool first
            if let Some(tx) = self.mempool.read().await.get(&txid_array) {
                let tx_bytes = bincode::serialize(&tx).map_err(|_| RpcError {
                    code: -8,
                    message: "Failed to serialize transaction".to_string(),
                })?;
                return Ok(json!(hex::encode(tx_bytes)));
            }

            // Use transaction index for O(1) lookup if available
            if let Some(ref tx_index) = self.blockchain.tx_index {
                if let Some(location) = tx_index.get_location(&txid_array) {
                    // Found in index - direct lookup
                    if let Ok(block) = self
                        .blockchain
                        .get_block_by_height(location.block_height)
                        .await
                    {
                        if let Some(tx) = block.transactions.get(location.tx_index) {
                            let tx_bytes = bincode::serialize(&tx).map_err(|_| RpcError {
                                code: -8,
                                message: "Failed to serialize transaction".to_string(),
                            })?;
                            return Ok(json!(hex::encode(tx_bytes)));
                        }
                    }
                }
            }

            // Fallback: Search blockchain
            let current_height = self.blockchain.get_height();

            for height in (0..=current_height).rev() {
                if let Ok(block) = self.blockchain.get_block_by_height(height).await {
                    for tx in &block.transactions {
                        if tx.txid() == txid_array {
                            let tx_bytes = bincode::serialize(&tx).map_err(|_| RpcError {
                                code: -8,
                                message: "Failed to serialize transaction".to_string(),
                            })?;
                            return Ok(json!(hex::encode(tx_bytes)));
                        }
                    }
                }
            }

            Err(RpcError {
                code: -5,
                message: "Transaction not found".to_string(),
            })
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
        // Start timevote consensus to finalize this transaction
        tokio::spawn({
            let consensus = self.consensus.clone();
            let tx_for_consensus = tx.clone();
            async move {
                // Initiate timevote consensus for transaction
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

            let total_balance: u64 = utxos
                .iter()
                .filter(|u| u.address == addr)
                .map(|u| u.value)
                .sum();

            let locked_balance: u64 = utxos
                .iter()
                .filter(|u| u.address == addr)
                .filter(|u| self.utxo_manager.is_collateral_locked(&u.outpoint))
                .map(|u| u.value)
                .sum();

            let available_balance = total_balance.saturating_sub(locked_balance);

            Ok(json!({
                "balance": total_balance as f64 / 100_000_000.0,
                "locked": locked_balance as f64 / 100_000_000.0,
                "available": available_balance as f64 / 100_000_000.0
            }))
        } else {
            // Get wallet balance for this masternode's reward address
            let utxos = self.utxo_manager.list_all_utxos().await;

            // Try to get this masternode's reward address
            if let Some(local_mn) = self.registry.get_local_masternode().await {
                let total_balance: u64 = utxos
                    .iter()
                    .filter(|u| u.address == local_mn.reward_address)
                    .map(|u| u.value)
                    .sum();

                let locked_balance: u64 = utxos
                    .iter()
                    .filter(|u| u.address == local_mn.reward_address)
                    .filter(|u| self.utxo_manager.is_collateral_locked(&u.outpoint))
                    .map(|u| u.value)
                    .sum();

                let available_balance = total_balance.saturating_sub(locked_balance);

                Ok(json!({
                    "balance": total_balance as f64 / 100_000_000.0,
                    "locked": locked_balance as f64 / 100_000_000.0,
                    "available": available_balance as f64 / 100_000_000.0
                }))
            } else {
                // Not a masternode - return 0 balance
                Ok(json!({
                    "balance": 0.0,
                    "locked": 0.0,
                    "available": 0.0
                }))
            }
        }
    }

    async fn list_unspent(&self, params: &[Value]) -> Result<Value, RpcError> {
        let _min_conf = params.first().and_then(|v| v.as_u64()).unwrap_or(1);
        let _max_conf = params.get(1).and_then(|v| v.as_u64()).unwrap_or(9999999);
        let addresses = params.get(2).and_then(|v| v.as_array());
        let limit = params.get(3).and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let utxos = self.utxo_manager.list_all_utxos().await;

        // Get local masternode's reward address to filter UTXOs
        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address);

        let mut filtered: Vec<Value> = utxos
            .iter()
            .filter(|u| {
                // First filter by local wallet address (only show this node's UTXOs)
                if let Some(ref local_addr) = local_address {
                    if u.address != *local_addr {
                        return false;
                    }
                } else {
                    // If not a masternode, don't show any UTXOs
                    return false;
                }

                // Then filter by specific addresses if provided
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

        // Sort by amount descending (largest first)
        filtered.sort_by(|a, b| {
            let amount_a = a.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let amount_b = b.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            amount_b.partial_cmp(&amount_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Apply limit (0 means no limit)
        let result = if limit > 0 && filtered.len() > limit {
            filtered.into_iter().take(limit).collect()
        } else {
            filtered
        };

        Ok(json!(result))
    }

    async fn list_received_by_address(&self, params: &[Value]) -> Result<Value, RpcError> {
        let minconf = params.first().and_then(|v| v.as_u64()).unwrap_or(1);
        let include_empty = params.get(1).and_then(|v| v.as_bool()).unwrap_or(false);

        let utxos = self.utxo_manager.list_all_utxos().await;

        // Get local masternode's reward address to filter UTXOs
        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address);

        // Group UTXOs by address
        use std::collections::HashMap;
        let mut address_map: HashMap<String, (u64, usize)> = HashMap::new();

        for utxo in utxos.iter() {
            // Only show this node's addresses
            if let Some(ref local_addr) = local_address {
                if utxo.address != *local_addr {
                    continue;
                }
            } else {
                // If not a masternode, don't show any addresses
                continue;
            }

            // Group by address (count amount and transaction count)
            let entry = address_map.entry(utxo.address.clone()).or_insert((0, 0));
            entry.0 += utxo.value;
            entry.1 += 1;
        }

        // Convert to JSON array
        let mut result: Vec<Value> = address_map
            .iter()
            .filter(|(_, (amount, _))| include_empty || *amount > 0)
            .map(|(address, (amount, txcount))| {
                json!({
                    "address": address,
                    "amount": *amount as f64 / 100_000_000.0,
                    "confirmations": minconf,
                    "txcount": txcount
                })
            })
            .collect();

        // Sort by amount descending
        result.sort_by(|a, b| {
            let amount_a = a.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let amount_b = b.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            amount_b
                .partial_cmp(&amount_a)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(json!(result))
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

        // timevote consensus parameters
        let timevote_config = json!({
            "protocol": "timevote + TSDC",
            "timevote": {
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
            "finality_type": "timevote consensus (seconds) + TimeLock Blocks (10 minutes)",
            "instant_finality": true,
            "average_finality_time_ms": 750
        });

        Ok(timevote_config)
    }

    /// Get timevote consensus status and metrics
    async fn get_timevote_status(&self) -> Result<Value, RpcError> {
        let masternodes = self.consensus.get_active_masternodes();
        let active_validators = masternodes.len();

        Ok(json!({
            "protocol": "timevote",
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
                "description": "timevote consensus: query random 20 validators per round, finalize after 15 consecutive confirms"
            },
            "note": "Transactions finalized by timevote in seconds, blocks produced every 10 minutes by TSDC"
        }))
    }

    async fn masternode_list(&self) -> Result<Value, RpcError> {
        let masternodes = self.registry.list_all().await;
        let list: Vec<Value> = masternodes
            .iter()
            .map(|mn| {
                // Phase 4.1: Check collateral status
                let (collateral_locked, collateral_outpoint) =
                    if let Some(ref outpoint) = mn.masternode.collateral_outpoint {
                        let locked = self.utxo_manager.is_collateral_locked(outpoint);
                        (
                            locked,
                            Some(format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout)),
                        )
                    } else {
                        (false, None)
                    };

                json!({
                    "address": mn.masternode.address,
                    "wallet_address": mn.masternode.wallet_address,
                    "collateral": mn.masternode.collateral,
                    "tier": format!("{:?}", mn.masternode.tier),
                    "registered_at": mn.masternode.registered_at,
                    "is_active": mn.is_active,
                    "uptime_start": mn.uptime_start,
                    "total_uptime": mn.total_uptime,
                    "collateral_locked": collateral_locked,
                    "collateral_outpoint": collateral_outpoint,
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
        let all_utxos = self.utxo_manager.list_all_utxos().await;

        // Filter to only spendable UTXOs (not locked as collateral)
        let mut utxos: Vec<_> = all_utxos
            .into_iter()
            .filter(|u| !self.utxo_manager.is_collateral_locked(&u.outpoint))
            .collect();

        if utxos.is_empty() {
            return Err(RpcError {
                code: -6,
                message:
                    "No spendable UTXOs available (check if all funds are locked as collateral)"
                        .to_string(),
            });
        }

        // Sort by value descending (use largest UTXOs first for efficiency)
        utxos.sort_by(|a, b| b.value.cmp(&a.value));

        // Estimate UTXOs needed and calculate fee (0.1% of total input value)
        let mut estimated_input = 0u64;
        let mut temp_fee = 1_000u64; // Start with minimum

        // Find UTXOs needed (including fee)
        for utxo in &utxos {
            estimated_input += utxo.value;
            temp_fee = (estimated_input / 1000).max(1_000);
            if estimated_input >= amount_units + temp_fee {
                break;
            }
        }

        let fee = temp_fee;

        // Find sufficient UTXOs
        let mut selected_utxos = Vec::new();
        let mut total_input = 0u64;

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
            // Send change back to the first input address
            let change_address = selected_utxos
                .first()
                .map(|u| u.address.as_bytes().to_vec())
                .unwrap_or_default();

            outputs.push(TxOutput {
                value: change,
                script_pubkey: change_address,
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
            Ok(_) => {
                // CRITICAL: Wait for instant finality before returning txid
                // This ensures the transaction is confirmed by masternodes
                let txid_hex = hex::encode(txid);

                tracing::info!("⏳ Waiting for transaction {} to finalize...", txid_hex);

                // Wait up to 30 seconds for finality
                let timeout = Duration::from_secs(30);
                let start = tokio::time::Instant::now();

                loop {
                    // Check if transaction is finalized
                    if self.consensus.tx_pool.is_finalized(&txid) {
                        tracing::info!("✅ Transaction {} finalized", txid_hex);
                        return Ok(json!(txid_hex));
                    }

                    // Check if transaction was rejected
                    if let Some(reason) = self.consensus.tx_pool.get_rejection_reason(&txid) {
                        tracing::warn!("❌ Transaction {} rejected: {}", txid_hex, reason);
                        return Err(RpcError {
                            code: -26,
                            message: format!("Transaction rejected during finality: {}", reason),
                        });
                    }

                    // Check timeout
                    if start.elapsed() > timeout {
                        tracing::warn!("⏰ Transaction {} finality timeout", txid_hex);
                        return Err(RpcError {
                            code: -26,
                            message: "Transaction finality timeout (30s) - transaction may still finalize later".to_string(),
                        });
                    }

                    // Wait a bit before checking again
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
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

        // Get local masternode's reward address
        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode".to_string(),
            })?
            .reward_address;

        // Filter to only this node's UTXOs, or specific address if provided
        if let Some(addr) = filter_address {
            utxos.retain(|utxo| utxo.address == addr);
        } else {
            utxos.retain(|utxo| utxo.address == local_address);
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

    // Removed: get_attestation_stats method (heartbeat functionality removed)
    // async fn get_attestation_stats(&self) -> Result<Value, RpcError> {
    //     ...
    // }

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
                "finality_type": "timevote"
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
                    "finality_type": "timevote",
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

    async fn get_best_block_hash(&self) -> Result<Value, RpcError> {
        let height = self.blockchain.get_height();
        match self.blockchain.get_block_by_height(height).await {
            Ok(block) => Ok(json!(hex::encode(block.hash()))),
            Err(_) => Err(RpcError {
                code: -1,
                message: "Block not found".to_string(),
            }),
        }
    }

    async fn get_block_hash(&self, params: &[Value]) -> Result<Value, RpcError> {
        let height = params
            .first()
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Block height parameter required".to_string(),
            })?;

        match self.blockchain.get_block_by_height(height).await {
            Ok(block) => Ok(json!(hex::encode(block.hash()))),
            Err(_) => Err(RpcError {
                code: -5,
                message: "Block not found".to_string(),
            }),
        }
    }

    async fn decode_raw_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let hex_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Hex-encoded transaction required".to_string(),
            })?;

        let tx_bytes = hex::decode(hex_str).map_err(|_| RpcError {
            code: -22,
            message: "Invalid hex encoding".to_string(),
        })?;

        let tx: Transaction = bincode::deserialize(&tx_bytes).map_err(|_| RpcError {
            code: -22,
            message: "Invalid transaction encoding".to_string(),
        })?;

        let txid = tx.txid();

        Ok(json!({
            "txid": hex::encode(txid),
            "version": tx.version,
            "locktime": tx.lock_time,
            "timestamp": tx.timestamp,
            "vin": tx.inputs.iter().map(|input| {
                json!({
                    "txid": hex::encode(input.previous_output.txid),
                    "vout": input.previous_output.vout,
                    "scriptSig": hex::encode(&input.script_sig),
                    "sequence": input.sequence
                })
            }).collect::<Vec<_>>(),
            "vout": tx.outputs.iter().enumerate().map(|(i, output)| {
                json!({
                    "value": output.value as f64 / 100_000_000.0,
                    "n": i,
                    "scriptPubKey": hex::encode(&output.script_pubkey)
                })
            }).collect::<Vec<_>>()
        }))
    }

    async fn get_new_address(&self, _params: &[Value]) -> Result<Value, RpcError> {
        // Get local masternode's reward address
        if let Some(local_mn) = self.registry.get_local_masternode().await {
            Ok(json!(local_mn.reward_address))
        } else {
            Err(RpcError {
                code: -4,
                message: "Node is not configured as a masternode. Cannot generate address."
                    .to_string(),
            })
        }
    }

    async fn get_wallet_info(&self) -> Result<Value, RpcError> {
        // Get local masternode info
        if let Some(local_mn) = self.registry.get_local_masternode().await {
            let utxos = self.utxo_manager.list_all_utxos().await;

            // Calculate total balance for this wallet
            let total_balance: u64 = utxos
                .iter()
                .filter(|u| u.address == local_mn.reward_address)
                .map(|u| u.value)
                .sum();

            // Calculate locked balance (collateral)
            let locked_balance: u64 = utxos
                .iter()
                .filter(|u| u.address == local_mn.reward_address)
                .filter(|u| self.utxo_manager.is_collateral_locked(&u.outpoint))
                .map(|u| u.value)
                .sum();

            // Available = total - locked
            let available_balance = total_balance.saturating_sub(locked_balance);

            let unconfirmed_balance = 0u64; // TIME has instant finality
            let immature_balance = 0u64;

            let utxo_count = utxos
                .iter()
                .filter(|u| u.address == local_mn.reward_address)
                .count();

            Ok(json!({
                "walletname": "default",
                "walletversion": 1,
                "format": "timecoin",
                "balance": total_balance as f64 / 100_000_000.0,
                "locked": locked_balance as f64 / 100_000_000.0,
                "available": available_balance as f64 / 100_000_000.0,
                "unconfirmed_balance": unconfirmed_balance as f64 / 100_000_000.0,
                "immature_balance": immature_balance as f64 / 100_000_000.0,
                "txcount": utxo_count,
                "keypoolsize": 1,
                "unlocked_until": 0,
                "paytxfee": 0.00001,
                "private_keys_enabled": true,
                "avoid_reuse": false,
                "scanning": false,
                "descriptors": false
            }))
        } else {
            Err(RpcError {
                code: -4,
                message: "Node is not configured as a masternode".to_string(),
            })
        }
    }

    /// Register a masternode with locked collateral
    /// Parameters: [tier, collateral_txid, vout, reward_address, node_address]
    /// Example: masternoderegister "bronze" "abc123..." 0 "wallet_addr" "node_addr"
    async fn masternode_register(&self, params: &[Value]) -> Result<Value, RpcError> {
        use crate::types::{Masternode, MasternodeTier};
        use ed25519_dalek::SigningKey;

        // Parse parameters
        let tier_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing tier parameter (bronze/silver/gold)".to_string(),
            })?;

        let collateral_txid = params
            .get(1)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing collateral_txid parameter".to_string(),
            })?;

        let vout = params
            .get(2)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing vout parameter".to_string(),
            })? as u32;

        let reward_address = params
            .get(3)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing reward_address parameter".to_string(),
            })?;

        let node_address = params
            .get(4)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing node_address parameter".to_string(),
            })?;

        // Parse tier
        let tier = match tier_str.to_lowercase().as_str() {
            "bronze" => MasternodeTier::Bronze,
            "silver" => MasternodeTier::Silver,
            "gold" => MasternodeTier::Gold,
            _ => {
                return Err(RpcError {
                    code: -32602,
                    message: "Invalid tier. Must be bronze, silver, or gold".to_string(),
                });
            }
        };

        // Get tier requirement (for validation logic, not returned)
        let _required_collateral = match tier {
            MasternodeTier::Free => 0,
            MasternodeTier::Bronze => 1_000 * 100_000_000, // 1,000 TIME in units
            MasternodeTier::Silver => 10_000 * 100_000_000, // 10,000 TIME in units
            MasternodeTier::Gold => 100_000 * 100_000_000, // 100,000 TIME in units
        };

        // Parse collateral outpoint
        let txid_bytes = hex::decode(collateral_txid).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid collateral_txid hex".to_string(),
        })?;

        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "collateral_txid must be 32 bytes".to_string(),
            });
        }

        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);

        let collateral_outpoint = OutPoint { txid, vout };

        // Get current block height
        let lock_height = self.blockchain.get_height();

        // Validate collateral using registry validation
        self.registry
            .validate_collateral(&collateral_outpoint, tier, &self.utxo_manager, lock_height)
            .await
            .map_err(|e| RpcError {
                code: -5,
                message: format!("Collateral validation failed: {}", e),
            })?;

        // Additional check: Verify UTXO belongs to reward address
        let utxo = self
            .utxo_manager
            .get_utxo(&collateral_outpoint)
            .await
            .map_err(|_| RpcError {
                code: -5,
                message: "Collateral UTXO not found".to_string(),
            })?;

        if utxo.address != reward_address {
            return Err(RpcError {
                code: -5,
                message: "Collateral UTXO does not belong to reward address".to_string(),
            });
        }

        // Generate masternode keypair
        use rand::rngs::OsRng;
        let mut csprng = OsRng;
        let signing_key = SigningKey::from_bytes(&rand::Rng::gen(&mut csprng));
        let public_key = signing_key.verifying_key();

        // Lock the UTXO atomically
        self.utxo_manager
            .lock_collateral(
                collateral_outpoint.clone(),
                node_address.to_string(),
                lock_height,
                utxo.value,
            )
            .map_err(|e| RpcError {
                code: -5,
                message: format!("Failed to lock collateral: {:?}", e),
            })?;

        // Create masternode with collateral
        let masternode = Masternode::new_with_collateral(
            node_address.to_string(),
            reward_address.to_string(),
            utxo.value,
            collateral_outpoint.clone(),
            public_key,
            tier,
            lock_height,
        );

        // Register with registry
        self.registry
            .register(masternode.clone(), reward_address.to_string())
            .await
            .map_err(|e| RpcError {
                code: -5,
                message: format!("Failed to register masternode: {:?}", e),
            })?;

        // Set as local masternode
        self.registry
            .set_local_masternode(node_address.to_string())
            .await;

        // Save signing key (in production, this should be saved securely)
        // For now, we'll return it to the user
        let signing_key_hex = hex::encode(signing_key.to_bytes());

        Ok(json!({
            "result": "success",
            "masternode_address": node_address,
            "reward_address": reward_address,
            "tier": format!("{:?}", tier),
            "collateral": utxo.value / 100_000_000,
            "collateral_outpoint": format!("{}:{}", hex::encode(collateral_outpoint.txid), collateral_outpoint.vout),
            "locked_at_height": lock_height,
            "public_key": hex::encode(public_key.to_bytes()),
            "signing_key": signing_key_hex,
            "message": "Masternode registered successfully. SAVE THE SIGNING KEY SECURELY!"
        }))
    }

    /// Unlock masternode collateral and deregister
    /// Parameters: [node_address] (optional, uses local if not provided)
    async fn masternode_unlock(&self, params: &[Value]) -> Result<Value, RpcError> {
        // Get node address
        let node_address = if let Some(addr) = params.first().and_then(|v| v.as_str()) {
            addr.to_string()
        } else {
            // Use local masternode
            self.registry
                .get_local_address()
                .await
                .ok_or_else(|| RpcError {
                    code: -4,
                    message: "No local masternode configured".to_string(),
                })?
        };

        // Get masternode info
        let mn_info = self
            .registry
            .get(&node_address)
            .await
            .ok_or_else(|| RpcError {
                code: -5,
                message: "Masternode not found".to_string(),
            })?;

        // Check if has locked collateral
        let collateral_outpoint =
            mn_info
                .masternode
                .collateral_outpoint
                .ok_or_else(|| RpcError {
                    code: -5,
                    message: "Masternode has no locked collateral (legacy masternode)".to_string(),
                })?;

        // Unlock the collateral
        self.utxo_manager
            .unlock_collateral(&collateral_outpoint)
            .map_err(|e| RpcError {
                code: -5,
                message: format!("Failed to unlock collateral: {:?}", e),
            })?;

        // Deregister the masternode
        self.registry
            .unregister(&node_address)
            .await
            .map_err(|e| RpcError {
                code: -5,
                message: format!("Failed to deregister masternode: {:?}", e),
            })?;

        // Broadcast unlock announcement to network
        use crate::network::message::NetworkMessage;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let unlock_msg = NetworkMessage::MasternodeUnlock {
            address: node_address.clone(),
            collateral_outpoint: collateral_outpoint.clone(),
            timestamp: now,
        };

        self.registry.broadcast_message(unlock_msg).await;

        Ok(json!({
            "result": "success",
            "masternode_address": node_address,
            "collateral_outpoint": format!("{}:{}", hex::encode(collateral_outpoint.txid), collateral_outpoint.vout),
            "message": "Masternode deregistered and collateral unlocked"
        }))
    }

    /// List all locked collaterals
    /// Returns all currently locked collaterals with masternode details
    async fn list_locked_collaterals(&self) -> Result<Value, RpcError> {
        let locked_collaterals = self.utxo_manager.list_locked_collaterals();

        let collaterals: Vec<_> = locked_collaterals
            .iter()
            .map(|lc| {
                json!({
                    "outpoint": format!("{}:{}", hex::encode(lc.outpoint.txid), lc.outpoint.vout),
                    "masternode_address": lc.masternode_address,
                    "amount": lc.amount,
                    "amount_time": format!("{:.8}", lc.amount as f64 / 100_000_000.0),
                    "lock_height": lc.lock_height,
                    "locked_at": lc.locked_at,
                    "unlock_height": lc.unlock_height,
                })
            })
            .collect();

        Ok(json!({
            "count": collaterals.len(),
            "collaterals": collaterals
        }))
    }

    async fn reindex_transactions(&self) -> Result<Value, RpcError> {
        // Check if transaction index is enabled
        if self.blockchain.tx_index.is_none() {
            return Err(RpcError {
                code: -1,
                message: "Transaction index not enabled".to_string(),
            });
        }

        // Trigger reindex in background (don't block RPC response)
        let blockchain = self.blockchain.clone();
        tokio::spawn(async move {
            tracing::info!("🔄 Starting transaction reindex...");
            match blockchain.build_tx_index().await {
                Ok(()) => {
                    tracing::info!("✅ Transaction reindex completed successfully");
                }
                Err(e) => {
                    tracing::error!("❌ Transaction reindex failed: {}", e);
                }
            }
        });

        Ok(json!({
            "message": "Transaction reindex started",
            "status": "running"
        }))
    }

    async fn get_tx_index_status(&self) -> Result<Value, RpcError> {
        if let Some((tx_count, height)) = self.blockchain.get_tx_index_stats() {
            Ok(json!({
                "enabled": true,
                "transactions_indexed": tx_count,
                "blockchain_height": height,
                "percent_indexed": if height > 0 {
                    (tx_count as f64 / (height as f64 * 10.0)) * 100.0  // Estimate ~10 txs/block
                } else {
                    0.0
                }
            }))
        } else {
            Ok(json!({
                "enabled": false,
                "message": "Transaction index not initialized"
            }))
        }
    }
}
