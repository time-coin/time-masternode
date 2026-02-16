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
    // Note: mempool field removed - use consensus.tx_pool instead for accurate state
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
            "masternodelist" => self.masternode_list(&params_array).await,
            "masternodestatus" => self.masternode_status().await,
            "listlockedcollaterals" => self.list_locked_collaterals().await,
            "getconsensusinfo" => self.get_consensus_info().await,
            "gettimevotestatus" => self.get_timevote_status().await,
            "validateaddress" => self.validate_address(&params_array).await,
            "stop" => self.stop().await,
            "uptime" => self.uptime().await,
            "getinfo" => self.get_info().await,
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
            "listtransactions" => self.list_transactions(&params_array).await,
            "reindextransactions" => self.reindex_transactions().await,
            "reindex" => self.reindex_full().await,
            "gettxindexstatus" => self.get_tx_index_status().await,
            "cleanuplockedutxos" => self.cleanup_locked_utxos().await,
            "listlockedutxos" => self.list_locked_utxos().await,
            "unlockutxo" => self.unlock_utxo(&params_array).await,
            "unlockorphanedutxos" => self.unlock_orphaned_utxos().await,
            "forceunlockall" => self.force_unlock_all().await,
            "gettransactions" => self.get_transactions_batch(&params_array).await,
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
            NetworkType::Mainnet => "mainnet",
            NetworkType::Testnet => "testnet",
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
            "consensus": "TimeVote + TimeLock",
            "finality_mechanism": "TimeVote consensus",
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
            "version": 110000, // 1.1.0
            "subversion": format!("/timed:{}/", env!("CARGO_PKG_VERSION")),
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
                    "subver": format!("/timed:{}/", env!("CARGO_PKG_VERSION")),
                    "inbound": false,
                    "conntime": mn.masternode.registered_at,
                    "timeoffset": 0,
                    "pingtime": pingtime,
                    "version": 110000, // 1.1.0
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
            message: format!(
                "Invalid txid format (expected 64 hex chars, got {} chars)",
                txid_str.len()
            ),
        })?;

        if txid.len() != 32 {
            return Err(RpcError {
                code: -8,
                message: format!(
                    "Invalid txid length (expected 32 bytes, got {})",
                    txid.len()
                ),
            });
        }

        // Check consensus tx_pool first (pending + finalized)
        let mut txid_array = [0u8; 32];
        txid_array.copy_from_slice(&txid);

        // Check transaction index FIRST (confirmed transactions take priority)
        // This avoids a race where the TX is still in the pool but already in a block
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

                        // Get wallet address for net amount calculation
                        let local_address = self
                            .registry
                            .get_local_masternode()
                            .await
                            .map(|mn| mn.reward_address);

                        // Calculate input/output sums and wallet-relative amounts
                        let mut input_sum: u64 = 0;
                        let mut wallet_input: u64 = 0;
                        let mut wallet_output: u64 = 0;
                        let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();

                        for output in &tx.outputs {
                            let addr = String::from_utf8_lossy(&output.script_pubkey);
                            if local_address.as_deref() == Some(addr.as_ref()) {
                                wallet_output += output.value;
                            }
                        }

                        for input in &tx.inputs {
                            if let Some(src_loc) =
                                tx_index.get_location(&input.previous_output.txid)
                            {
                                if let Ok(src_block) =
                                    self.blockchain.get_block(src_loc.block_height)
                                {
                                    if let Some(src_tx) =
                                        src_block.transactions.get(src_loc.tx_index)
                                    {
                                        if let Some(src_out) =
                                            src_tx.outputs.get(input.previous_output.vout as usize)
                                        {
                                            input_sum += src_out.value;
                                            let src_addr =
                                                String::from_utf8_lossy(&src_out.script_pubkey);
                                            if local_address.as_deref() == Some(src_addr.as_ref()) {
                                                wallet_input += src_out.value;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let fee = if input_sum > 0 {
                            input_sum.saturating_sub(output_sum)
                        } else {
                            0
                        };

                        // Net amount: positive = received, negative = sent
                        let net_amount = if wallet_input > 0 {
                            (wallet_output as i64) - (wallet_input as i64)
                        } else {
                            wallet_output as i64
                        };

                        // Look up TimeProof certificate
                        let timeproof_json = self.consensus.finality_proof_mgr
                            .get_timeproof(&txid_array)
                            .map(|proof| json!({
                                "votes": proof.votes.len(),
                                "slot_index": proof.slot_index,
                                "accumulated_weight": proof.votes.iter().map(|v| v.voter_weight).sum::<u64>(),
                            }));

                        let mut result = json!({
                            "txid": hex::encode(txid_array),
                            "version": tx.version,
                            "size": bincode::serialize(tx).map(|v| v.len()).unwrap_or(250),
                            "locktime": tx.lock_time,
                            "amount": net_amount as f64 / 100_000_000.0,
                            "fee": fee as f64 / 100_000_000.0,
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
                        });

                        if let Some(tp) = timeproof_json {
                            result["timeproof"] = tp;
                        }

                        return Ok(result);
                    }
                }
            }
        }

        // Then check pool (pending/finalized but not yet in a block)
        if let Some(tx) = self.consensus.tx_pool.get_transaction(&txid_array) {
            let is_finalized = self.consensus.tx_pool.is_finalized(&txid_array);
            let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();

            // Get wallet address for net amount calculation
            let local_address = self
                .registry
                .get_local_masternode()
                .await
                .map(|mn| mn.reward_address);

            let mut wallet_input: u64 = 0;
            let mut wallet_output: u64 = 0;

            for output in &tx.outputs {
                let addr = String::from_utf8_lossy(&output.script_pubkey);
                if local_address.as_deref() == Some(addr.as_ref()) {
                    wallet_output += output.value;
                }
            }

            // Try to calculate fee from input UTXOs
            let mut input_sum: u64 = 0;
            if let Some(ref txi) = self.blockchain.tx_index {
                for input in &tx.inputs {
                    if let Some(src_loc) = txi.get_location(&input.previous_output.txid) {
                        if let Ok(src_block) = self.blockchain.get_block(src_loc.block_height) {
                            if let Some(src_tx) = src_block.transactions.get(src_loc.tx_index) {
                                if let Some(src_out) =
                                    src_tx.outputs.get(input.previous_output.vout as usize)
                                {
                                    input_sum += src_out.value;
                                    let src_addr = String::from_utf8_lossy(&src_out.script_pubkey);
                                    if local_address.as_deref() == Some(src_addr.as_ref()) {
                                        wallet_input += src_out.value;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let fee = if input_sum > 0 {
                input_sum.saturating_sub(output_sum)
            } else {
                0
            };

            let net_amount = if wallet_input > 0 {
                (wallet_output as i64) - (wallet_input as i64)
            } else {
                wallet_output as i64
            };

            // Look up TimeProof certificate
            let timeproof_json = self.consensus.finality_proof_mgr
                .get_timeproof(&txid_array)
                .map(|proof| json!({
                    "votes": proof.votes.len(),
                    "slot_index": proof.slot_index,
                    "accumulated_weight": proof.votes.iter().map(|v| v.voter_weight).sum::<u64>(),
                }));

            let mut result = json!({
                "txid": hex::encode(txid_array),
                "version": tx.version,
                "size": 250, // Estimate
                "locktime": tx.lock_time,
                "amount": net_amount as f64 / 100_000_000.0,
                "fee": fee as f64 / 100_000_000.0,
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
                "finalized": is_finalized,
                "time": tx.timestamp,
                "blocktime": tx.timestamp
            });

            if let Some(tp) = timeproof_json {
                result["timeproof"] = tp;
            }

            return Ok(result);
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

            // Check consensus tx_pool first
            if let Some(tx) = self.consensus.tx_pool.get_transaction(&txid_array) {
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

        // Transaction is already submitted to consensus via consensus.submit_transaction
        // in sendtoaddress RPC, so we don't need to add to mempool here
        // The consensus engine manages the tx_pool internally

        // Process transaction through consensus
        // Start TimeVote consensus to finalize this transaction
        let txid_hex = hex::encode(txid);
        tracing::info!("📤 Submitting transaction {} to consensus", &txid_hex[..16]);
        tokio::spawn({
            let consensus = self.consensus.clone();
            let tx_for_consensus = tx.clone();
            let txid_for_log = txid_hex.clone();
            async move {
                // Initiate TimeVote consensus for transaction
                match consensus.add_transaction(tx_for_consensus).await {
                    Ok(_) => {
                        tracing::info!(
                            "✅ Transaction {} accepted by consensus",
                            &txid_for_log[..16]
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "❌ Transaction {} REJECTED by consensus: {}",
                            &txid_for_log[..16],
                            e
                        );
                    }
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

        let utxos = self.utxo_manager.list_all_utxos().await;

        let filter_addr = if let Some(addr) = address {
            addr.to_string()
        } else if let Some(local_mn) = self.registry.get_local_masternode().await {
            local_mn.reward_address
        } else {
            return Ok(json!({
                "balance": 0.0,
                "locked": 0.0,
                "available": 0.0
            }));
        };

        let mut spendable: u64 = 0;
        let mut locked_collateral: u64 = 0;
        let mut pending: u64 = 0;

        for u in utxos.iter().filter(|u| u.address == filter_addr) {
            if self.utxo_manager.is_collateral_locked(&u.outpoint) {
                locked_collateral += u.value;
                continue;
            }
            match self.utxo_manager.get_state(&u.outpoint) {
                Some(crate::types::UTXOState::Unspent) => spendable += u.value,
                _ => pending += u.value,
            }
        }

        let total = spendable + locked_collateral + pending;

        Ok(json!({
            "balance": total as f64 / 100_000_000.0,
            "locked": locked_collateral as f64 / 100_000_000.0,
            "available": spendable as f64 / 100_000_000.0
        }))
    }

    async fn list_unspent(&self, params: &[Value]) -> Result<Value, RpcError> {
        // Default min_conf=0: TIME Coin has instant finality via TimeVote,
        // so finalized transaction outputs should be visible immediately
        let min_conf = params.first().and_then(|v| v.as_u64()).unwrap_or(0);
        let max_conf = params.get(1).and_then(|v| v.as_u64()).unwrap_or(9999999);
        let addresses = params.get(2).and_then(|v| v.as_array());
        let limit = params.get(3).and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let utxos = self.utxo_manager.list_all_utxos().await;
        let current_height = self.blockchain.get_height();

        // Get local masternode's reward address to filter UTXOs
        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address);

        let local_addr = match &local_address {
            Some(addr) => addr.clone(),
            None => return Ok(json!([])),
        };

        // Collect txids already in the on-chain UTXO set to avoid duplicates
        let mut seen_outpoints: std::collections::HashSet<(Vec<u8>, u32)> =
            std::collections::HashSet::new();

        let mut filtered: Vec<Value> = utxos
            .iter()
            .filter(|u| {
                // First filter by local wallet address (only show this node's UTXOs)
                if u.address != local_addr {
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
                seen_outpoints.insert((u.outpoint.txid.to_vec(), u.outpoint.vout));

                // Get UTXO state
                let state = self.utxo_manager.get_state(&u.outpoint);
                let is_locked = self.utxo_manager.is_collateral_locked(&u.outpoint);

                let (spendable, state_str) = match state {
                    Some(crate::types::UTXOState::Unspent) if !is_locked => (true, "unspent"),
                    Some(crate::types::UTXOState::Unspent) if is_locked => {
                        (false, "collateral_locked")
                    }
                    Some(crate::types::UTXOState::Locked { .. }) => (false, "locked"),
                    Some(crate::types::UTXOState::SpentPending { .. }) => (false, "spending"),
                    Some(crate::types::UTXOState::SpentFinalized { .. }) => (false, "spent"),
                    Some(crate::types::UTXOState::Confirmed { .. }) => (false, "confirmed"),
                    None => (false, "unknown"),
                    _ => (false, "unavailable"),
                };

                let confirmations = self
                    .blockchain
                    .tx_index
                    .as_ref()
                    .and_then(|idx| idx.get_location(&u.outpoint.txid))
                    .map(|loc| current_height.saturating_sub(loc.block_height) + 1)
                    .unwrap_or(0);

                json!({
                    "txid": hex::encode(u.outpoint.txid),
                    "vout": u.outpoint.vout,
                    "address": u.address,
                    "amount": u.value as f64 / 100_000_000.0,
                    "confirmations": confirmations,
                    "spendable": spendable,
                    "state": state_str,
                    "solvable": true,
                    "safe": true
                })
            })
            .filter(|v| {
                let c = v.get("confirmations").and_then(|v| v.as_u64()).unwrap_or(0);
                c >= min_conf && c <= max_conf
            })
            .collect();

        // Include outputs from finalized transactions not yet in a block.
        // TIME Coin achieves instant finality via TimeVote consensus (67% threshold),
        // so finalized transaction outputs are safe to display before block inclusion.
        if min_conf == 0 {
            let finalized_txs = self.consensus.tx_pool.get_finalized_transactions();
            for tx in &finalized_txs {
                let txid = tx.txid();
                for (vout, output) in tx.outputs.iter().enumerate() {
                    // Decode address from script_pubkey
                    let output_address = String::from_utf8_lossy(&output.script_pubkey).to_string();
                    if output_address != local_addr {
                        continue;
                    }
                    // Filter by specific addresses if provided
                    if let Some(addrs) = addresses {
                        if !addrs
                            .iter()
                            .any(|a| a.as_str() == Some(output_address.as_str()))
                        {
                            continue;
                        }
                    }
                    // Skip if already in the on-chain UTXO set
                    if seen_outpoints.contains(&(txid.to_vec(), vout as u32)) {
                        continue;
                    }
                    filtered.push(json!({
                        "txid": hex::encode(txid),
                        "vout": vout,
                        "address": output_address,
                        "amount": output.value as f64 / 100_000_000.0,
                        "confirmations": 0,
                        "spendable": false,
                        "state": "finalized",
                        "solvable": true,
                        "safe": true
                    }));
                }
            }
        }

        // Sort by amount descending (largest first)
        filtered.sort_by(|a, b| {
            let amount_a = a.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let amount_b = b.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
            amount_b
                .partial_cmp(&amount_a)
                .unwrap_or(std::cmp::Ordering::Equal)
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
        let current_height = self.blockchain.get_height();

        // Get local masternode's reward address to filter UTXOs
        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address);

        // Group UTXOs by address: (total_amount, tx_count, min_confirmations)
        use std::collections::HashMap;
        let mut address_map: HashMap<String, (u64, usize, u64)> = HashMap::new();

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

            let confirmations = self
                .blockchain
                .tx_index
                .as_ref()
                .and_then(|idx| idx.get_location(&utxo.outpoint.txid))
                .map(|loc| current_height.saturating_sub(loc.block_height) + 1)
                .unwrap_or(0);

            let entry = address_map
                .entry(utxo.address.clone())
                .or_insert((0, 0, u64::MAX));
            entry.0 += utxo.value;
            entry.1 += 1;
            entry.2 = entry.2.min(confirmations);
        }

        // Convert to JSON array
        let mut result: Vec<Value> = address_map
            .iter()
            .filter(|(_, (amount, _, confs))| (include_empty || *amount > 0) && *confs >= minconf)
            .map(|(address, (amount, txcount, confs))| {
                json!({
                    "address": address,
                    "amount": *amount as f64 / 100_000_000.0,
                    "confirmations": confs,
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

    /// List recent transactions involving this wallet (sent and received).
    /// Params: [count (default 10)]
    async fn list_transactions(&self, params: &[Value]) -> Result<Value, RpcError> {
        let count = params.first().and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address)
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode".to_string(),
            })?;

        let chain_height = self.blockchain.get_height();
        let mut transactions: Vec<Value> = Vec::new();

        // Scan blocks from newest to oldest, collecting wallet-related TXs
        let scan_start = chain_height;
        for height in (0..=scan_start).rev() {
            if transactions.len() >= count {
                break;
            }

            let block = match self.blockchain.get_block(height) {
                Ok(b) => b,
                Err(_) => continue,
            };

            let block_hash = hex::encode(block.hash());
            let block_time = block.header.timestamp;

            for (tx_idx, tx) in block.transactions.iter().enumerate() {
                let txid = hex::encode(tx.txid());

                // Check if any output goes to our address (receive)
                let mut received: u64 = 0;
                for output in &tx.outputs {
                    let addr = String::from_utf8_lossy(&output.script_pubkey);
                    if addr == local_address {
                        received += output.value;
                    }
                }

                // Check if any input spends from our address (send)
                let mut sent: u64 = 0;
                for input in &tx.inputs {
                    // Look up the UTXO being spent to check its address
                    let spent_txid = input.previous_output.txid;
                    let spent_vout = input.previous_output.vout;

                    // Search for the source transaction in the chain
                    if let Some(ref txi) = self.blockchain.tx_index {
                        if let Some(loc) = txi.get_location(&spent_txid) {
                            if let Ok(src_block) = self.blockchain.get_block(loc.block_height) {
                                if let Some(src_tx) = src_block.transactions.get(loc.tx_index) {
                                    if let Some(src_output) =
                                        src_tx.outputs.get(spent_vout as usize)
                                    {
                                        let src_addr =
                                            String::from_utf8_lossy(&src_output.script_pubkey);
                                        if src_addr == local_address {
                                            sent += src_output.value;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if sent > 0 || received > 0 {
                    // Skip coinbase (tx_idx 0) and reward distribution (tx_idx 1) for "send"
                    // They are always "receive" type
                    let category = if tx_idx <= 1 {
                        "generate"
                    } else if sent > 0 && received > 0 {
                        // Change back to self — net effect is a send
                        "send"
                    } else if sent > 0 {
                        "send"
                    } else {
                        "receive"
                    };

                    let net_amount = if category == "send" {
                        // For sends, show the net amount leaving the wallet (negative)
                        // sent - received = total input from wallet - change back
                        -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                    } else {
                        received as f64 / 100_000_000.0
                    };

                    // Calculate fee for sends
                    let fee = if category == "send" {
                        let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                        let total_in = sent; // We only know our inputs
                        if total_in > total_out {
                            Some(-((total_in - total_out) as f64 / 100_000_000.0))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let mut entry = json!({
                        "txid": txid,
                        "category": category,
                        "amount": net_amount,
                        "confirmations": chain_height.saturating_sub(height) + 1,
                        "blockhash": block_hash,
                        "blockheight": height,
                        "blocktime": block_time,
                        "time": block_time,
                    });

                    if let Some(f) = fee {
                        entry["fee"] = json!(f);
                    }

                    transactions.push(entry);
                }
            }
        }

        // Truncate to requested count
        transactions.truncate(count);

        Ok(json!(transactions))
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
                "public_key": hex::encode(local_mn.masternode.public_key.to_bytes()),
                "version": env!("CARGO_PKG_VERSION"),
                "git_hash": option_env!("GIT_HASH").unwrap_or("unknown")
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
        // Get real mempool info from consensus engine
        let (pending_count, finalized_count) = self.consensus.get_mempool_info();
        let total_count = pending_count + finalized_count;

        // Estimate bytes (250 bytes per transaction is reasonable average)
        let bytes = total_count * 250;

        Ok(json!({
            "loaded": true,
            "size": total_count,
            "pending": pending_count,
            "finalized": finalized_count,
            "bytes": bytes,
            "usage": bytes,
            "maxmempool": 300000000,
            "mempoolminfee": 0.00001,
            "minrelaytxfee": 0.00001
        }))
    }

    async fn get_raw_mempool(&self) -> Result<Value, RpcError> {
        // Get transaction IDs from consensus tx_pool
        let pending_txs = self.consensus.tx_pool.get_pending_transactions();
        let finalized_txs = self.consensus.tx_pool.get_finalized_transactions();

        let mut txids: Vec<String> = Vec::new();
        for tx in pending_txs {
            txids.push(hex::encode(tx.txid()));
        }
        for tx in finalized_txs {
            txids.push(hex::encode(tx.txid()));
        }

        Ok(json!(txids))
    }

    async fn get_consensus_info(&self) -> Result<Value, RpcError> {
        let masternodes = self.consensus.get_active_masternodes();
        let mn_count = masternodes.len();

        // Filter to only masternodes on the consensus chain
        let consensus_peers = self.blockchain.get_consensus_peers().await;
        let on_chain_count = if consensus_peers.is_empty() {
            // No consensus data yet — count all active as fallback
            mn_count
        } else {
            masternodes
                .iter()
                .filter(|mn| {
                    let ip = mn
                        .address
                        .split(':')
                        .next()
                        .unwrap_or(&mn.address);
                    consensus_peers.iter().any(|p| p == ip)
                })
                .count()
                // +1 for ourselves (we're not in the peer list but are on our own chain)
                + 1
        };

        // TimeVote consensus parameters
        let timevote_config = json!({
            "protocol": "TimeVote + TimeLock",
            "timevote": {
                "sample_size": 20,
                "finality_confidence": 15,
                "query_timeout_ms": 2000,
                "description": "Instant transaction finality via random validator sampling"
            },
            "timelock": {
                "block_time_seconds": 600,
                "leader_selection": "Verifiable Random Function (VRF)",
                "description": "Deterministic 10-minute block production"
            },
            "active_validators": on_chain_count,
            "finality_type": "TimeVote consensus (seconds) + TimeLock blocks (10 minutes)",
            "instant_finality": true,
            "average_finality_time_ms": self.consensus.get_avg_finality_time_ms()
        });

        Ok(timevote_config)
    }

    /// Get TimeVote consensus status and metrics
    async fn get_timevote_status(&self) -> Result<Value, RpcError> {
        let masternodes = self.consensus.get_active_masternodes();
        let active_validators = masternodes.len();

        Ok(json!({
            "protocol": "TimeVote",
            "status": "active",
            "active_validators": active_validators,
            "configuration": {
                "sample_size": 20,
                "finality_threshold": 15,
                "query_timeout_ms": 2000,
                "max_rounds": 100
            },
            "metrics": {
                "average_finality_time_ms": self.consensus.get_avg_finality_time_ms(),
                "finality_type": "probabilistic (cryptographically secure)",
                "validator_sampling": "random k-of-n",
                "description": "TimeVote consensus: query random 20 validators per round, finalize after 15 consecutive confirms"
            },
            "note": "Transactions finalized by TimeVote in seconds, blocks produced every 10 minutes by TimeLock"
        }))
    }

    async fn masternode_list(&self, params: &[Value]) -> Result<Value, RpcError> {
        // Parse show_all parameter (defaults to false - only show connected)
        let show_all = params.first().and_then(|v| v.as_bool()).unwrap_or(false);

        let all_masternodes = self.registry.list_all().await;

        // Get connection manager and peer registry to check connection status
        let connection_manager = self.blockchain.get_connection_manager().await;
        let peer_registry = self.blockchain.get_peer_registry().await;

        // Build full list with connection status
        let full_list: Vec<_> = all_masternodes
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

                // Check if masternode is currently connected (check both registries)
                let ip_only = mn
                    .masternode
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&mn.masternode.address);
                let cm_connected = connection_manager
                    .as_ref()
                    .map(|cm| cm.is_connected(ip_only))
                    .unwrap_or(false);
                let pr_connected = peer_registry
                    .as_ref()
                    .map(|pr| pr.is_connected(ip_only))
                    .unwrap_or(false);
                let is_connected = cm_connected || pr_connected;

                (mn, is_connected, collateral_locked, collateral_outpoint)
            })
            .collect();

        // Filter to connected only if show_all is false
        let filtered_list: Vec<Value> = full_list
            .iter()
            .filter(|(_, is_connected, _, _)| show_all || *is_connected)
            .map(
                |(mn, is_connected, collateral_locked, collateral_outpoint)| {
                    json!({
                        "address": mn.masternode.address,
                        "wallet_address": mn.masternode.wallet_address,
                        "collateral": mn.masternode.collateral as f64 / 100_000_000.0,
                        "tier": format!("{:?}", mn.masternode.tier),
                        "registered_at": mn.masternode.registered_at,
                        "is_active": mn.is_active,
                        "is_connected": is_connected,
                        "uptime_start": mn.uptime_start,
                        "total_uptime": mn.total_uptime,
                        "collateral_locked": collateral_locked,
                        "collateral_outpoint": collateral_outpoint,
                    })
                },
            )
            .collect();

        Ok(json!({
            "total": filtered_list.len(),
            "total_in_registry": all_masternodes.len(),
            "show_all": show_all,
            "masternodes": filtered_list
        }))
    }

    async fn uptime(&self) -> Result<Value, RpcError> {
        let uptime = SystemTime::now()
            .duration_since(self.start_time)
            .unwrap()
            .as_secs();
        Ok(json!(uptime))
    }

    async fn get_info(&self) -> Result<Value, RpcError> {
        // Get blockchain info
        let height = self.blockchain.get_height();

        // Get masternode count
        let masternodes = self.registry.active_count().await;

        // Get balance
        let all_utxos = self.utxo_manager.list_all_utxos().await;
        let balance: u64 = all_utxos.iter().map(|u| u.value).sum();
        let balance_time = balance as f64 / 100_000_000.0;

        // Get uptime
        let uptime = SystemTime::now()
            .duration_since(self.start_time)
            .unwrap()
            .as_secs();

        // Get version
        let version = env!("CARGO_PKG_VERSION");

        Ok(json!({
            "version": version,
            "blocks": height,
            "masternodes": masternodes,
            "balance": balance_time,
            "uptime": uptime,
            "network": format!("{:?}", self.network),
        }))
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

        // Optional 3rd param: subtract_fee_from_amount (default: false)
        let subtract_fee = params.get(2).and_then(|v| v.as_bool()).unwrap_or(false);

        // Optional 4th param: nowait - return TXID immediately without waiting for finality
        let nowait = params.get(3).and_then(|v| v.as_bool()).unwrap_or(false);

        // Convert TIME to smallest unit (like satoshis)
        let amount_units = (amount * 100_000_000.0) as u64;

        // Get wallet address for UTXO filtering and change output
        let wallet_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address)
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode - no wallet address".to_string(),
            })?;

        // On UTXO contention, exclude contested outpoints and re-select different UTXOs
        const MAX_RETRIES: u32 = 3;
        let mut excluded: std::collections::HashSet<OutPoint> = std::collections::HashSet::new();
        let mut last_error = String::new();

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                tracing::info!(
                    "🔄 Retry {}/{} — selecting different UTXOs ({} excluded)",
                    attempt,
                    MAX_RETRIES,
                    excluded.len()
                );
            }

            // Get UTXOs for this wallet (fresh each attempt)
            let all_utxos = self.utxo_manager.list_all_utxos().await;

            // Filter: our address, unspent, not collateral, not in exclusion set
            let mut utxos: Vec<_> = all_utxos
                .into_iter()
                .filter(|u| {
                    if u.address != wallet_address {
                        return false;
                    }
                    if excluded.contains(&u.outpoint) {
                        return false;
                    }
                    if self.utxo_manager.is_collateral_locked(&u.outpoint) {
                        return false;
                    }
                    matches!(
                        self.utxo_manager.get_state(&u.outpoint),
                        Some(crate::types::UTXOState::Unspent)
                    )
                })
                .collect();

            if utxos.is_empty() {
                if excluded.is_empty() {
                    return Err(RpcError {
                        code: -6,
                        message:
                            "No spendable UTXOs available (all funds may be locked or in use by pending transactions)"
                                .to_string(),
                    });
                }
                // All remaining UTXOs are excluded — contention too high
                return Err(RpcError {
                    code: -6,
                    message: format!(
                        "No spendable UTXOs available after excluding {} contested outputs",
                        excluded.len()
                    ),
                });
            }

            // Sort by value descending (use largest UTXOs first for efficiency)
            utxos.sort_by(|a, b| b.value.cmp(&a.value));

            // Estimate fee
            let mut estimated_input = 0u64;
            let mut temp_fee = 1_000u64;
            for utxo in &utxos {
                estimated_input += utxo.value;
                temp_fee = (estimated_input / 1000).max(1_000);
                let needed = if subtract_fee {
                    amount_units
                } else {
                    amount_units + temp_fee
                };
                if estimated_input >= needed {
                    break;
                }
            }
            let fee = temp_fee;

            // Select sufficient UTXOs
            let mut selected_utxos = Vec::new();
            let mut total_input = 0u64;
            for utxo in &utxos {
                selected_utxos.push(utxo.clone());
                total_input += utxo.value;
                let needed = if subtract_fee {
                    amount_units
                } else {
                    amount_units + fee
                };
                if total_input >= needed {
                    break;
                }
            }

            let send_amount = if subtract_fee {
                if total_input < amount_units {
                    return Err(RpcError {
                        code: -6,
                        message: "Insufficient funds".to_string(),
                    });
                }
                let fee = (total_input / 1000).max(1_000);
                if amount_units <= fee {
                    return Err(RpcError {
                        code: -6,
                        message: format!("Amount too small to cover fee ({} units fee)", fee),
                    });
                }
                amount_units - fee
            } else {
                if total_input < amount_units + fee {
                    return Err(RpcError {
                        code: -6,
                        message: "Insufficient funds".to_string(),
                    });
                }
                amount_units
            };

            let inputs: Vec<TxInput> = selected_utxos
                .iter()
                .map(|utxo| TxInput {
                    previous_output: utxo.outpoint.clone(),
                    script_sig: vec![],
                    sequence: 0xFFFFFFFF,
                })
                .collect();

            let mut outputs = vec![TxOutput {
                value: send_amount,
                script_pubkey: to_address.as_bytes().to_vec(),
            }];

            let change = total_input - send_amount - fee;
            if change > 0 {
                outputs.push(TxOutput {
                    value: change,
                    script_pubkey: wallet_address.as_bytes().to_vec(),
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

            match self.consensus.submit_transaction(tx).await {
                Ok(_) => {
                    let txid_hex = hex::encode(txid);

                    if attempt > 0 {
                        tracing::info!(
                            "✅ Transaction {} succeeded on retry {}",
                            &txid_hex[..16],
                            attempt
                        );
                    }

                    if nowait {
                        tracing::info!("📤 Transaction {} broadcast (nowait)", txid_hex);
                        return Ok(json!(txid_hex));
                    }

                    tracing::info!("⏳ Waiting for transaction {} to finalize...", txid_hex);

                    let timeout = Duration::from_secs(30);
                    let start = tokio::time::Instant::now();

                    loop {
                        if self.consensus.tx_pool.is_finalized(&txid) {
                            tracing::info!("✅ Transaction {} finalized", txid_hex);
                            return Ok(json!(txid_hex));
                        }

                        if let Some(reason) = self.consensus.tx_pool.get_rejection_reason(&txid) {
                            tracing::warn!("❌ Transaction {} rejected: {}", txid_hex, reason);
                            return Err(RpcError {
                                code: -26,
                                message: format!(
                                    "Transaction rejected during finality: {}",
                                    reason
                                ),
                            });
                        }

                        if start.elapsed() > timeout {
                            tracing::warn!("⏰ Transaction {} finality timeout", txid_hex);
                            return Err(RpcError {
                                code: -26,
                                message: "Transaction finality timeout (30s) - transaction may still finalize later".to_string(),
                            });
                        }

                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
                Err(e) => {
                    let is_contention = e.contains("double-spend prevented")
                        || e.contains("AlreadyLocked")
                        || e.contains("already locked")
                        || e.contains("in use by");
                    if is_contention && attempt < MAX_RETRIES {
                        // Exclude the contested UTXOs so next attempt picks different ones
                        for utxo in &selected_utxos {
                            excluded.insert(utxo.outpoint.clone());
                        }
                        tracing::warn!(
                            "⚠️ UTXO contention (attempt {}): {} — excluding {} outpoints",
                            attempt + 1,
                            e,
                            selected_utxos.len()
                        );
                        last_error = e;
                        continue;
                    }
                    return Err(RpcError {
                        code: -26,
                        message: format!("Transaction rejected: {}", e),
                    });
                }
            }
        }

        // All retries exhausted
        Err(RpcError {
            code: -26,
            message: format!(
                "Transaction failed after {} retries due to UTXO contention: {}",
                MAX_RETRIES, last_error
            ),
        })
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

        // Filter out collateral locked and non-Unspent UTXOs
        utxos.retain(|u| {
            // Must not be collateral locked
            if self.utxo_manager.is_collateral_locked(&u.outpoint) {
                return false;
            }
            // Must be in Unspent state
            matches!(
                self.utxo_manager.get_state(&u.outpoint),
                Some(crate::types::UTXOState::Unspent)
            )
        });

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
                "finality_type": "TimeVote"
            }));
        }

        // Check if transaction is in consensus tx_pool
        if self.consensus.tx_pool.has_transaction(&txid_array) {
            let is_finalized = self.consensus.tx_pool.is_finalized(&txid_array);
            return Ok(json!({
                "txid": txid,
                "finalized": is_finalized,
                "status": if is_finalized { "finalized" } else { "pending" },
                "in_mempool": true
            }));
        }

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
                    "finality_type": "TimeVote",
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

            // Categorize UTXOs by state
            let mut spendable_balance: u64 = 0;
            let mut locked_collateral: u64 = 0;
            let mut pending_balance: u64 = 0;
            let mut utxo_count: usize = 0;

            for u in utxos
                .iter()
                .filter(|u| u.address == local_mn.reward_address)
            {
                utxo_count += 1;

                if self.utxo_manager.is_collateral_locked(&u.outpoint) {
                    locked_collateral += u.value;
                    continue;
                }

                match self.utxo_manager.get_state(&u.outpoint) {
                    Some(crate::types::UTXOState::Unspent) => {
                        spendable_balance += u.value;
                    }
                    Some(
                        crate::types::UTXOState::Locked { .. }
                        | crate::types::UTXOState::SpentPending { .. },
                    ) => {
                        pending_balance += u.value;
                    }
                    _ => {
                        // SpentFinalized, Confirmed, etc. — shouldn't be in storage
                        // but count them as non-spendable if present
                        pending_balance += u.value;
                    }
                }
            }

            let total_balance = spendable_balance + locked_collateral + pending_balance;

            Ok(json!({
                "walletname": "default",
                "walletversion": 1,
                "format": "timecoin",
                "balance": total_balance as f64 / 100_000_000.0,
                "locked": locked_collateral as f64 / 100_000_000.0,
                "available": spendable_balance as f64 / 100_000_000.0,
                "pending": pending_balance as f64 / 100_000_000.0,
                "unconfirmed_balance": pending_balance as f64 / 100_000_000.0,
                "immature_balance": 0.0,
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

    /// Full reindex: clear UTXOs and rebuild from block 0, plus rebuild tx index.
    /// This fixes stale wallet balances after chain corruption or reset.
    /// Runs synchronously so the CLI gets the result directly.
    async fn reindex_full(&self) -> Result<Value, RpcError> {
        let blockchain = self.blockchain.clone();
        let height = blockchain.get_height();

        tracing::info!(
            "🔄 Starting full reindex (UTXOs + transactions) for {} blocks...",
            height
        );

        // Step 1: Reindex UTXOs from block 0 (synchronous — caller waits for result)
        let (blocks, utxos) = match blockchain.reindex_utxos().await {
            Ok((blocks, utxos)) => {
                tracing::info!(
                    "✅ UTXO reindex complete: {} blocks, {} UTXOs",
                    blocks,
                    utxos
                );
                (blocks, utxos)
            }
            Err(e) => {
                tracing::error!("❌ UTXO reindex failed: {}", e);
                return Err(RpcError {
                    code: -1,
                    message: format!("UTXO reindex failed: {}", e),
                });
            }
        };

        // Step 2: Rebuild transaction index
        let tx_indexed = match blockchain.build_tx_index().await {
            Ok(()) => {
                tracing::info!("✅ Transaction reindex completed");
                true
            }
            Err(e) => {
                tracing::warn!(
                    "⚠️  Transaction reindex failed (tx_index may not be enabled): {}",
                    e
                );
                false
            }
        };

        tracing::info!("✅ Full reindex complete");

        Ok(json!({
            "message": "Full reindex complete",
            "status": "complete",
            "chain_height": height,
            "blocks_processed": blocks,
            "utxo_count": utxos,
            "tx_index_rebuilt": tx_indexed
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

    /// Cleanup expired UTXO locks (older than 10 minutes)
    /// Returns the number of locks cleaned up
    async fn cleanup_locked_utxos(&self) -> Result<Value, RpcError> {
        let cleaned = self.utxo_manager.cleanup_expired_locks();

        Ok(json!({
            "cleaned": cleaned,
            "message": format!("Cleaned {} expired UTXO locks", cleaned)
        }))
    }

    /// List all currently locked UTXOs with details
    async fn list_locked_utxos(&self) -> Result<Value, RpcError> {
        let now = chrono::Utc::now().timestamp();

        // Get locked UTXOs directly from the state map
        let locked_list = self.utxo_manager.get_locked_utxos();

        let mut locked: Vec<Value> = Vec::new();

        for (outpoint, txid, locked_at) in locked_list {
            // Try to get UTXO details from storage
            if let Ok(utxo) = self.utxo_manager.get_utxo(&outpoint).await {
                let age_seconds = now - locked_at;
                let expired = age_seconds > 600; // 10 minutes

                locked.push(json!({
                    "txid": hex::encode(outpoint.txid),
                    "vout": outpoint.vout,
                    "address": utxo.address,
                    "amount": utxo.value as f64 / 100_000_000.0,
                    "locked_by_tx": hex::encode(txid),
                    "locked_at": locked_at,
                    "age_seconds": age_seconds,
                    "expired": expired
                }));
            } else {
                // UTXO not in storage but has a lock state - orphaned state
                let age_seconds = now - locked_at;
                let expired = age_seconds > 600;

                locked.push(json!({
                    "txid": hex::encode(outpoint.txid),
                    "vout": outpoint.vout,
                    "address": "Unknown (orphaned state)",
                    "amount": 0.0,
                    "locked_by_tx": hex::encode(txid),
                    "locked_at": locked_at,
                    "age_seconds": age_seconds,
                    "expired": expired,
                    "orphaned": true
                }));
            }
        }

        Ok(json!({
            "locked_count": locked.len(),
            "locked_utxos": locked
        }))
    }

    /// Manually unlock a specific UTXO by txid and vout
    /// Parameters: [txid, vout]
    async fn unlock_utxo(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected txid".to_string(),
            })?;

        let vout = params
            .get(1)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected vout".to_string(),
            })? as u32;

        let txid_bytes = hex::decode(txid_str).map_err(|_| RpcError {
            code: -8,
            message: "Invalid txid format".to_string(),
        })?;

        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -8,
                message: "Invalid txid length".to_string(),
            });
        }

        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);

        let outpoint = crate::types::OutPoint { txid, vout };

        // Check current state
        match self.utxo_manager.get_state(&outpoint) {
            Some(crate::types::UTXOState::Locked {
                txid: lock_txid,
                locked_at,
            }) => {
                // Unlock it
                self.utxo_manager
                    .update_state(&outpoint, crate::types::UTXOState::Unspent);

                Ok(json!({
                    "unlocked": true,
                    "txid": txid_str,
                    "vout": vout,
                    "was_locked_by": hex::encode(lock_txid),
                    "was_locked_at": locked_at,
                    "message": "UTXO unlocked successfully"
                }))
            }
            Some(state) => Err(RpcError {
                code: -8,
                message: format!("UTXO is not locked, current state: {}", state),
            }),
            None => Err(RpcError {
                code: -8,
                message: "UTXO not found".to_string(),
            }),
        }
    }

    /// Scan for orphaned locks (where the locking transaction doesn't exist) and unlock them
    async fn unlock_orphaned_utxos(&self) -> Result<Value, RpcError> {
        let utxos = self.utxo_manager.list_all_utxos().await;
        let mut unlocked_count = 0;
        let mut orphaned = Vec::new();

        for utxo in utxos {
            if let Some(crate::types::UTXOState::Locked { txid, locked_at }) =
                self.utxo_manager.get_state(&utxo.outpoint)
            {
                // Check if the locking transaction exists in consensus pool or blockchain
                let tx_exists = self.consensus.tx_pool.has_transaction(&txid);

                if !tx_exists {
                    // Transaction doesn't exist - this is an orphaned lock
                    tracing::info!(
                        "Unlocking orphaned UTXO {:?} (locked by non-existent tx {})",
                        utxo.outpoint,
                        hex::encode(txid)
                    );

                    self.utxo_manager
                        .update_state(&utxo.outpoint, crate::types::UTXOState::Unspent);
                    unlocked_count += 1;

                    orphaned.push(json!({
                        "txid": hex::encode(utxo.outpoint.txid),
                        "vout": utxo.outpoint.vout,
                        "amount": utxo.value as f64 / 100_000_000.0,
                        "locked_by_missing_tx": hex::encode(txid),
                        "locked_at": locked_at
                    }));
                }
            }
        }

        Ok(json!({
            "unlocked": unlocked_count,
            "orphaned_utxos": orphaned,
            "message": format!("Unlocked {} orphaned UTXOs", unlocked_count)
        }))
    }

    /// Force unlock ALL locked UTXOs (nuclear option for recovery)
    /// This resets all UTXOs to Unspent state
    async fn force_unlock_all(&self) -> Result<Value, RpcError> {
        let all_utxos = self.utxo_manager.list_all_utxos().await;
        let mut unlocked_count = 0;

        for utxo in all_utxos {
            if self.utxo_manager.force_unlock(&utxo.outpoint) {
                unlocked_count += 1;
            }
        }

        tracing::warn!(
            "⚠️  Force unlocked {} UTXOs to Unspent state",
            unlocked_count
        );

        Ok(json!({
            "unlocked": unlocked_count,
            "message": format!("Force unlocked all {} UTXOs", unlocked_count)
        }))
    }

    /// Batch query transaction status for multiple txids.
    /// Params: [["txid1", "txid2", ...]] or ["txid1", "txid2", ...]
    async fn get_transactions_batch(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txids: Vec<&str> = if let Some(arr) = params.first().and_then(|v| v.as_array()) {
            arr.iter().filter_map(|v| v.as_str()).collect()
        } else {
            params.iter().filter_map(|v| v.as_str()).collect()
        };

        if txids.is_empty() {
            return Err(RpcError {
                code: -32602,
                message: "Invalid params: expected array of txids".to_string(),
            });
        }

        if txids.len() > 100 {
            return Err(RpcError {
                code: -32602,
                message: "Too many txids (max 100 per batch)".to_string(),
            });
        }

        let current_height = self.blockchain.get_height();
        let mut results = Vec::with_capacity(txids.len());

        for txid_str in txids {
            let txid = match hex::decode(txid_str) {
                Ok(t) if t.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&t);
                    arr
                }
                _ => {
                    results.push(json!({
                        "txid": txid_str,
                        "error": "invalid txid format"
                    }));
                    continue;
                }
            };

            // Check transaction index (confirmed in block)
            if let Some(ref tx_index) = self.blockchain.tx_index {
                if let Some(location) = tx_index.get_location(&txid) {
                    let confirmations = current_height - location.block_height + 1;
                    let timeproof_json = self
                        .consensus
                        .finality_proof_mgr
                        .get_timeproof(&txid)
                        .map(|proof| {
                            json!({
                                "votes": proof.votes.len(),
                                "slot_index": proof.slot_index,
                                "accumulated_weight": proof.votes.iter().map(|v| v.voter_weight).sum::<u64>(),
                            })
                        });
                    let mut entry = json!({
                        "txid": txid_str,
                        "finalized": true,
                        "confirmations": confirmations,
                    });
                    if let Some(tp) = timeproof_json {
                        entry["timeproof"] = tp;
                    }
                    results.push(entry);
                    continue;
                }
            }

            // Check pool (pending/finalized but not yet in block)
            let is_finalized = self.consensus.tx_pool.is_finalized(&txid);
            if self.consensus.tx_pool.get_transaction(&txid).is_some() {
                let timeproof_json = self
                    .consensus
                    .finality_proof_mgr
                    .get_timeproof(&txid)
                    .map(|proof| {
                        json!({
                            "votes": proof.votes.len(),
                            "slot_index": proof.slot_index,
                            "accumulated_weight": proof.votes.iter().map(|v| v.voter_weight).sum::<u64>(),
                        })
                    });
                let mut entry = json!({
                    "txid": txid_str,
                    "finalized": is_finalized,
                    "confirmations": 0,
                });
                if let Some(tp) = timeproof_json {
                    entry["timeproof"] = tp;
                }
                results.push(entry);
                continue;
            }

            results.push(json!({
                "txid": txid_str,
                "error": "not found"
            }));
        }

        Ok(json!({ "transactions": results }))
    }
}
