//! RPC method implementations for the TIME Coin JSON-RPC 2.0 server.
//!
//! All public methods are routed through [`RpcHandler::handle_request`].
//!
//! ## Bitcoin compatibility
//!
//! Methods that share names with Bitcoin Core's RPC API follow the same parameter
//! conventions and response shapes wherever possible, so that standard Bitcoin
//! tooling (bitcoin-cli, libraries, block explorers) can be pointed at a TIME
//! node with minimal adaptation.
//!
//! ### Implemented Bitcoin-compatible methods
//!
//! | Method                  | Notes |
//! |-------------------------|-------|
//! | `getblockchaininfo`     | |
//! | `getblockcount`         | |
//! | `getblock`              | Accepts **hash** (64-char hex) **or** height (number) |
//! | `getblockheader`        | Accepts hash or height; returns header without tx list |
//! | `getbestblockhash`      | |
//! | `getblockhash`          | |
//! | `gettxout`              | Returns `null` for spent/unknown outputs |
//! | `getrawtransaction`     | `verbose=true` returns full JSON |
//! | `gettransaction`        | |
//! | `sendrawtransaction`    | |
//! | `createrawtransaction`  | |
//! | `decoderawtransaction`  | |
//! | `testmempoolaccept`     | Validates tx without broadcasting |
//! | `getmempoolinfo`        | |
//! | `getrawmempool`         | `verbose=true` returns full entry objects |
//! | `estimatesmartfee`      | Uses live `FeeSchedule`; always `blocks=1` (instant finality) |
//! | `getnetworkinfo`        | |
//! | `getpeerinfo`           | |
//! | `getconnectioncount`    | |
//! | `gettxoutsetinfo`       | |
//! | `getbalance`            | |
//! | `getbalances`           | |
//! | `listunspent`           | |
//! | `getnewaddress`         | |
//! | `getwalletinfo`         | |
//! | `validateaddress`       | |
//! | `getaddressinfo`        | Modern superset of `validateaddress`; sets `ismine` correctly |
//! | `sendtoaddress`         | |
//! | `sendfrom`              | Deprecated in Bitcoin ≥0.15, kept for compat |
//! | `listreceivedbyaddress` | |
//! | `listtransactions`      | |
//! | `signmessage`           | Ed25519; only the local node address is supported |
//! | `verifymessage`         | Looks up pubkey from on-chain UTXO index |
//! | `lockunspent`           | Bitcoin-compat UTXO lock/unlock toggle |
//! | `listlockunspent`       | Bitcoin-compat alias for `listlockedutxos` |
//! | `uptime`                | |
//! | `stop`                  | |
//! | `getinfo`               | Deprecated in Bitcoin ≥0.16, kept for compat |
//!
//! ### TIME-specific extensions
//!
//! | Method                     | Purpose |
//! |----------------------------|---------|
//! | `getconsensusinfo`         | TimeVote consensus state |
//! | `gettimevotestatus`        | Per-slot voting progress |
//! | `gettransactionfinality`   | Finality proof for a txid |
//! | `waittransactionfinality`  | Long-poll until finalized |
//! | `masternodelist`           | All registered masternodes |
//! | `masternodestatus`         | Local node masternode status |
//! | `masternodegenkey`         | Generate a new masternode key |
//! | `masternodereginfo`        | Registration requirements |
//! | `masternoderegstatus`      | Check registration eligibility |
//! | `listlockedcollaterals`    | Collateral UTXOs locked for masternodes |
//! | `gettreasurybalance`       | Treasury fund balance |
//! | `getfeeschedule`           | Current tiered fee schedule |
//! | `mergeutxos`               | Consolidate many small UTXOs |
//! | `listlockedutxos`          | All locked UTXOs with details |
//! | `unlockutxo`               | Manually unlock a specific UTXO |
//! | `unlockorphanedutxos`      | Unlock UTXOs locked by missing txs |
//! | `forceunlockall`           | Emergency: reset all UTXO locks |
//! | `clearstucktransactions`   | Recovery: roll back stuck finalized txs |
//! | `cleanuplockedutxos`       | Remove expired UTXO locks |
//! | `listtransactionsmulti`    | `listtransactions` across multiple addresses |
//! | `listunspentmulti`         | `listunspent` across multiple addresses |
//! | `gettransactions`          | Batch txid status query (up to 100) |
//! | `reindextransactions`      | Rebuild the tx index (async) |
//! | `reindex`                  | Full UTXO + tx index rebuild |
//! | `gettxindexstatus`         | Tx index health check |
//! | `createpaymentrequest`     | Create a signed payment request URI |
//! | `sendpaymentrequest`       | Deliver a payment request to the payer |
//! | `paypaymentrequest`        | Pay an incoming request |
//! | `getpaymentrequests`       | List payment requests for an address |
//! | `acknowledgepaymentrequest`| Payer acknowledges receipt |
//! | `respondpaymentrequest`    | Payer accepts or declines |
//! | `cancelpaymentrequest`     | Requester cancels |
//! | `markpaymentrequestviewed` | Payer marks as viewed |
//! | `submitproposal`           | Submit a governance proposal |
//! | `voteproposal`             | Vote on a governance proposal |
//! | `listproposals`            | List governance proposals |
//! | `getproposal`              | Get proposal detail and vote tally |
//!
//! ### Known gaps vs Bitcoin Core (not yet implemented)
//!
//! - `getblockstats` — per-block statistics
//! - `getchaintips` — chain tip / fork detection
//! - `getmempoolancestors` / `getmempooldescendants`
//! - `signrawtransactionwithkey` — offline signing with an explicit WIF key
//! - `decodescript` — decode a raw script
//! - `addnode` / `disconnectnode` / `getaddednodeinfo` — peer management
//! - `getnettotals` — network bandwidth counters
//! - `dumpprivkey` / `importprivkey` — key import/export

#![allow(dead_code)]

use super::server::{RpcError, RpcRequest, RpcResponse};
use crate::address::Address;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::types::{OutPoint, Transaction, TxInput, TxOutput};
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use base64::Engine as _;
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
    /// Broadcast channel for notifying WebSocket clients of new transactions
    tx_event_sender:
        Option<tokio::sync::broadcast::Sender<crate::rpc::websocket::TransactionEvent>>,
    reconnection_ai: Option<Arc<crate::ai::adaptive_reconnection::AdaptiveReconnectionAI>>,
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
            tx_event_sender: None,
            reconnection_ai: None,
        }
    }

    /// Set the transaction event broadcast sender for WebSocket notifications
    pub fn set_tx_event_sender(
        &mut self,
        sender: tokio::sync::broadcast::Sender<crate::rpc::websocket::TransactionEvent>,
    ) {
        self.tx_event_sender = Some(sender);
    }

    pub fn set_reconnection_ai(
        &mut self,
        ai: Arc<crate::ai::adaptive_reconnection::AdaptiveReconnectionAI>,
    ) {
        self.reconnection_ai = Some(ai);
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
            "rebroadcasttransaction" => self.rebroadcast_transaction(&params_array).await,
            "createrawtransaction" => self.create_raw_transaction(&params_array).await,
            "decoderawtransaction" => self.decode_raw_transaction(&params_array).await,
            "getbalance" => self.get_balance(&params_array).await,
            "listunspent" => self.list_unspent(&params_array).await,
            "getnewaddress" => self.get_new_address(&params_array).await,
            "getwalletinfo" => self.get_wallet_info().await,
            "masternodelist" => self.masternode_list(&params_array).await,
            "masternoderemove" => self.masternode_remove(&params_array).await,
            "masternodestatus" => self.masternode_status().await,
            "checkcollateral" => self.check_collateral().await,
            "findcollateral" => self.find_collateral(&params_array).await,
            "listlockedcollaterals" => self.list_locked_collaterals().await,
            "unlockcollateral" => self.unlock_collateral_rpc(&params_array).await,
            "getconsensusinfo" => self.get_consensus_info().await,
            "gettimevotestatus" => self.get_timevote_status().await,
            "validateaddress" => self.validate_address(&params_array).await,
            "getaddresspubkey" => self.get_address_pubkey(&params_array).await,
            "registeraddresspubkey" => self.register_address_pubkey(&params_array).await,
            "stop" => self.stop().await,
            "uptime" => self.uptime().await,
            "getinfo" => self.get_info().await,
            "getmempoolinfo" => self.get_mempool_info().await,
            "getrawmempool" => self.get_raw_mempool(&params_array).await,
            "getmempoolverbose" => self.get_mempool_verbose().await,
            "sendtoaddress" => self.send_to_address(&params_array).await,
            "sendfrom" => self.send_from(&params_array).await,
            "mergeutxos" => self.merge_utxos(&params_array).await,
            "gettransactionfinality" => self.get_transaction_finality(&params_array).await,
            "waittransactionfinality" => self.wait_transaction_finality(&params_array).await,
            "getwhitelist" => self.get_whitelist().await,
            "addwhitelist" => self.add_whitelist(&params_array).await,
            "removewhitelist" => self.remove_whitelist(&params_array).await,
            "getblacklist" => self.get_blacklist().await,
            "ban" => self.ban_ip(&params_array).await,
            "unban" => self.unban(&params_array).await,
            "unbansubnet" => self.unban_subnet(&params_array).await,
            "clearcollateralanchor" => self.clear_collateral_anchor(&params_array).await,
            "clearbanlist" => self.clear_ban_list().await,
            "resetpeerprofiles" => self.reset_peer_profiles().await,
            "auditcollateral" => self.audit_collateral().await,
            "listreceivedbyaddress" => self.list_received_by_address(&params_array).await,
            "listtransactions" => self.list_transactions(&params_array).await,
            "listtransactionsmulti" => self.list_transactions_multi(&params_array).await,
            "reindextransactions" => self.reindex_transactions().await,
            "reindex" => self.reindex_full().await,
            "rollbacktoblock0" => self.rollback_to_block0().await,
            "rollbacktoheight" => self.rollback_to_height_rpc(&params_array).await,
            "resyncfromwhitelist" => self.resync_from_whitelist_rpc(&params_array).await,
            "resetfinalitylock" => self.reset_finality_lock_rpc(&params_array).await,
            "gettxindexstatus" => self.get_tx_index_status().await,
            "cleanuplockedutxos" => self.cleanup_locked_utxos().await,
            "listlockedutxos" => self.list_locked_utxos().await,
            "unlockutxo" => self.unlock_utxo(&params_array).await,
            "unlockorphanedutxos" => self.unlock_orphaned_utxos().await,
            "forceunlockall" => self.force_unlock_all().await,
            "releaseallcollaterals" => self.release_all_collaterals().await,
            "gettransactions" => self.get_transactions_batch(&params_array).await,
            "gettreasurybalance" => self.get_treasury_balance().await,
            "getbalances" => self.get_balances(&params_array).await,
            "listunspentmulti" => self.list_unspent_multi(&params_array).await,
            "masternodegenkey" => self.masternode_genkey().await,
            "getfeeschedule" => self.get_fee_schedule().await,
            "masternodereginfo" => self.masternode_reg_info().await,
            "masternoderegstatus" => self.masternode_reg_status(&params_array).await,
            "clearstucktransactions" => self.clear_stuck_transactions().await,
            "droptransaction" => self.drop_transaction(&params_array).await,
            "createpaymentrequest" => self.create_payment_request(&params_array).await,
            "paypaymentrequest" => self.pay_payment_request(&params_array).await,
            "sendpaymentrequest" => self.send_payment_request(&params_array).await,
            "getpaymentrequests" => self.get_payment_requests(&params_array).await,
            "acknowledgepaymentrequest" => self.acknowledge_payment_request(&params_array).await,
            "respondpaymentrequest" => self.respond_payment_request(&params_array).await,
            "cancelpaymentrequest" => self.cancel_payment_request(&params_array).await,
            "markpaymentrequestviewed" => self.mark_payment_request_viewed(&params_array).await,
            "submitproposal" => self.submit_proposal(&params_array).await,
            "voteproposal" => self.vote_proposal(&params_array).await,
            "listproposals" => self.list_proposals(&params_array).await,
            "getproposal" => self.get_proposal(&params_array).await,
            // --- Bitcoin-compatible additions ---
            "getblockheader" => self.get_block_header(&params_array).await,
            "getpeerblock" => self.get_peer_block(&params_array).await,
            "gettxout" => self.get_txout(&params_array).await,
            "testmempoolaccept" => self.test_mempool_accept(&params_array).await,
            "estimatesmartfee" => self.estimate_smart_fee(&params_array).await,
            "getaddressinfo" => self.get_address_info(&params_array).await,
            "getconnectioncount" => self.get_connection_count().await,
            "signmessage" => self.sign_message(&params_array).await,
            "verifymessage" => self.verify_message(&params_array).await,
            "lockunspent" => self.lock_unspent(&params_array).await,
            "listlockunspent" => self.list_lock_unspent().await,
            "submitcollateralproof" => self.submit_collateral_proof(&params_array).await,
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
        let is_syncing = self.blockchain.is_syncing();

        // Get real average finality time from consensus engine
        let avg_finality_ms = self.consensus.get_avg_finality_time_ms();

        // Best-effort sync progress: ask the peer registry for the highest
        // known peer tip so wallets can show a progress estimate.
        // Timeout prevents this from blocking the RPC thread during heavy fork resolution.
        let peer_tip = tokio::time::timeout(Duration::from_secs(2), async {
            if let Some(registry) = self.blockchain.get_peer_registry().await {
                let peers = registry.get_connected_peers().await;
                let mut max_tip = height;
                for peer in &peers {
                    if let Some(h) = registry.get_peer_height(peer).await {
                        if h > max_tip {
                            max_tip = h;
                        }
                    }
                }
                max_tip
            } else {
                height
            }
        })
        .await
        .unwrap_or(height);
        let verification_progress = if is_syncing && peer_tip > 0 {
            (height as f64 / peer_tip as f64).min(1.0)
        } else {
            1.0
        };

        Ok(json!({
            "chain": chain,
            "blocks": height,
            "headers": peer_tip,
            "bestblockhash": hex::encode(best_hash),
            "difficulty": 1.0,
            "mediantime": chrono::Utc::now().timestamp(),
            "verificationprogress": verification_progress,
            "initialblockdownload": is_syncing,
            "chainwork": format!("{:064x}", height),
            "pruned": false,
            "consensus": "TimeVote + TimeLock",
            "finality_mechanism": "TimeVote consensus",
            "instant_finality": true,
            "average_finality_time_ms": avg_finality_ms,
            "block_time_seconds": 600,
            "emission_satoshis_per_block": self.blockchain.get_current_block_reward(),
            "emission_time_per_block": self.blockchain.get_current_block_reward() as f64 / 100_000_000.0,
            "storage_version": crate::constants::STORAGE_VERSION
        }))
    }

    async fn get_block_count(&self) -> Result<Value, RpcError> {
        let height = self.blockchain.get_height();
        Ok(json!(height))
    }

    async fn get_block(&self, params: &[Value]) -> Result<Value, RpcError> {
        let first = params.first().ok_or_else(|| RpcError {
            code: -32602,
            message: "Expected block hash (string) or height (number)".to_string(),
        })?;

        // Accept either a 64-char hex hash string or a numeric height
        let block = if let Some(hash_str) = first.as_str() {
            let hash_bytes = hex::decode(hash_str).map_err(|_| RpcError {
                code: -8,
                message: "Invalid block hash encoding".to_string(),
            })?;
            if hash_bytes.len() != 32 {
                return Err(RpcError {
                    code: -8,
                    message: "Block hash must be 32 bytes (64 hex chars)".to_string(),
                });
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&hash_bytes);
            self.find_block_by_hash(hash)
                .await
                .ok_or_else(|| RpcError {
                    code: -5,
                    message: "Block not found".to_string(),
                })?
        } else if let Some(height) = first.as_u64() {
            self.blockchain
                .get_block_by_height(height)
                .await
                .map_err(|e| RpcError {
                    code: -5,
                    message: format!("Block not found: {}", e),
                })?
        } else {
            return Err(RpcError {
                code: -32602,
                message: "Expected block hash (string) or height (number)".to_string(),
            });
        };

        let height = block.header.height;
        let txids: Vec<String> = block
            .transactions
            .iter()
            .map(|tx| hex::encode(tx.txid()))
            .collect();
        let block_hash = block.hash();

        Ok(json!({
            "height": height,
            "hash": hex::encode(block_hash),
            "previousblockhash": hex::encode(block.header.previous_hash),
            "time": block.header.timestamp,
            "version": block.header.version,
            "merkleroot": hex::encode(block.header.merkle_root),
            "tx": txids,
            "nTx": block.transactions.len(),
            "confirmations": (self.blockchain.get_height() as i64 - height as i64 + 1).max(0),
            "block_reward": block.header.block_reward,
            "fees": block.header.total_fees,
            "masternode_rewards": block.masternode_rewards.iter().map(|(addr, amount)| {
                json!({ "address": addr, "amount": amount })
            }).collect::<Vec<_>>(),
        }))
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
        let peer_registry = self.blockchain.get_peer_registry().await;

        let mut peers: Vec<Value> = Vec::with_capacity(masternodes.len());
        for mn in &masternodes {
            // Look up the peer's actual reported height and ping time from the
            // connection registry.  Previously this was gated behind is_active
            // (gossip liveness), which meant newly-connected peers showed no
            // height or ping until 3+ peers had gossiped about them.  Now we
            // always check the registry — if we have a live connection, show it.
            let (height, pingtime) = if let Some(ref pr) = peer_registry {
                let h = pr
                    .get_peer_height(&mn.masternode.address)
                    .await
                    .unwrap_or(0);
                let p = pr.get_peer_ping_time(&mn.masternode.address).await;
                (h, p)
            } else {
                (0u64, None)
            };

            peers.push(json!({
                "addr": mn.masternode.address.clone(),
                "services": "0000000000000409",
                "lastseen": mn.masternode.registered_at,
                "subver": format!("/timed:{}/", env!("CARGO_PKG_VERSION")),
                "inbound": false,
                "conntime": mn.masternode.registered_at,
                "timeoffset": 0,
                "pingtime": pingtime,
                "version": 110000,
                "storage_version": crate::constants::STORAGE_VERSION,
                "is_masternode": true,
                "tier": format!("{:?}", mn.masternode.tier),
                "active": mn.is_active,
                "height": height,
            }));
        }
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

    /// Serialize `special_data` for RPC / explorer responses.
    /// Returns `null` for regular transactions so the field is always present
    /// and clients can distinguish special TXs from ghost TXs.
    fn special_data_to_json(tx: &crate::types::Transaction) -> Value {
        use crate::types::SpecialTransactionData;
        match &tx.special_data {
            None => Value::Null,
            Some(SpecialTransactionData::MasternodeRegistration {
                node_address,
                wallet_address,
                reward_address,
                collateral_outpoint,
                pubkey,
                ..
            }) => json!({
                "type": "MasternodeRegistration",
                "node_address": node_address,
                "wallet_address": wallet_address,
                "reward_address": reward_address,
                "collateral_outpoint": collateral_outpoint,
                "pubkey": pubkey,
            }),
            Some(SpecialTransactionData::MasternodeDeregistration {
                node_address,
                slot_id,
                pubkey,
                ..
            }) => json!({
                "type": "MasternodeDeregistration",
                "node_address": node_address,
                "slot_id": slot_id,
                "pubkey": pubkey,
            }),
            Some(SpecialTransactionData::MasternodePayoutUpdate {
                node_address,
                new_reward_address,
                pubkey,
                ..
            }) => json!({
                "type": "MasternodePayoutUpdate",
                "node_address": node_address,
                "new_reward_address": new_reward_address,
                "pubkey": pubkey,
            }),
        }
    }

    /// Convert a raw `script_pubkey` to a human-readable address string.
    /// Coinbase outputs use `b"BLOCK_REWARD_"` + height bytes; display them as
    /// `"(coinbase)"` instead of letting the binary height bytes corrupt the string.
    fn script_pubkey_to_address(script: &[u8]) -> String {
        if script.starts_with(b"BLOCK_REWARD_") {
            return "(coinbase)".to_string();
        }
        String::from_utf8_lossy(script).into_owned()
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
                            let addr = Self::script_pubkey_to_address(&output.script_pubkey);
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
                                            let src_addr = Self::script_pubkey_to_address(
                                                &src_out.script_pubkey,
                                            );
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
                            "vout": tx.outputs.iter().enumerate().map(|(i, output)| {
                                let addr = Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                                json!({
                                    "value": output.value as f64 / 100_000_000.0,
                                    "n": i,
                                    "address": addr.clone(),
                                    "scriptPubKey": {
                                        "hex": hex::encode(&output.script_pubkey),
                                        "address": addr
                                    }
                                })
                            }).collect::<Vec<_>>(),
                            "confirmations": confirmations,
                            "time": tx.timestamp,
                            "blocktime": block.header.timestamp,
                            "blockhash": hex::encode(block.hash()),
                            "height": location.block_height,
                            "special_data": Self::special_data_to_json(tx),
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
                let addr = Self::script_pubkey_to_address(&output.script_pubkey);
                if local_address.as_deref() == Some(addr.as_ref()) {
                    wallet_output += output.value;
                }
            }

            // Try to calculate fee from input UTXOs.
            // Check tx_index (archived) first, then fall back to the live UTXO manager
            // for inputs that haven't been archived in a block yet.  Without the fallback,
            // pending transactions always show fee = 0 because their inputs are unarchived.
            let mut input_sum: u64 = 0;
            for input in &tx.inputs {
                let mut found = false;
                // 1. tx_index — fastest, covers archived inputs
                if let Some(ref txi) = self.blockchain.tx_index {
                    if let Some(src_loc) = txi.get_location(&input.previous_output.txid) {
                        if let Ok(src_block) = self.blockchain.get_block(src_loc.block_height) {
                            if let Some(src_tx) = src_block.transactions.get(src_loc.tx_index) {
                                if let Some(src_out) =
                                    src_tx.outputs.get(input.previous_output.vout as usize)
                                {
                                    input_sum += src_out.value;
                                    let src_addr =
                                        Self::script_pubkey_to_address(&src_out.script_pubkey);
                                    if local_address.as_deref() == Some(src_addr.as_ref()) {
                                        wallet_input += src_out.value;
                                    }
                                    found = true;
                                }
                            }
                        }
                    }
                }
                // 2. Live UTXO manager — covers unarchived (pending/finalized) inputs
                if !found {
                    if let Ok(utxo) = self
                        .consensus
                        .utxo_manager
                        .get_utxo(&input.previous_output)
                        .await
                    {
                        input_sum += utxo.value;
                        if local_address.as_deref() == Some(utxo.address.as_str()) {
                            wallet_input += utxo.value;
                        }
                    }
                }
            }

            let fee = input_sum.saturating_sub(output_sum);

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
                "vout": tx.outputs.iter().enumerate().map(|(i, output)| {
                    let addr = Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                    json!({
                        "value": output.value as f64 / 100_000_000.0,
                        "n": i,
                        "address": addr.clone(),
                        "scriptPubKey": {
                            "hex": hex::encode(&output.script_pubkey),
                            "address": addr
                        }
                    })
                }).collect::<Vec<_>>(),
                "confirmations": 0,
                "finalized": is_finalized,
                "time": tx.timestamp,
                "blocktime": tx.timestamp,
                "special_data": Self::special_data_to_json(&tx),
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
                                        "address": Self::script_pubkey_to_address(&output.script_pubkey).to_string()
                                    }
                                })).collect::<Vec<_>>(),
                                "confirmations": confirmations,
                                "time": tx.timestamp,
                                "blocktime": block.header.timestamp,
                                "blockhash": hex::encode(block.hash()),
                                "height": height,
                                "special_data": Self::special_data_to_json(tx),
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
        let param = params.first().ok_or_else(|| RpcError {
            code: -32602,
            message: "Invalid params: expected transaction object or hex".to_string(),
        })?;

        // Accept either a JSON object (from mobile wallet) or a hex-encoded
        // bincode string (legacy desktop wallet format).
        let tx: Transaction = if param.is_object() || param.is_array() {
            serde_json::from_value(param.clone()).map_err(|e| RpcError {
                code: -22,
                message: format!("TX parse failed: {}", e),
            })?
        } else {
            let hex_tx = param.as_str().ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected transaction object or hex string".to_string(),
            })?;
            let tx_bytes = hex::decode(hex_tx).map_err(|_| RpcError {
                code: -22,
                message: "TX decode failed".to_string(),
            })?;
            bincode::deserialize(&tx_bytes).map_err(|_| RpcError {
                code: -22,
                message: "TX deserialization failed".to_string(),
            })?
        };

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

        // Emit WebSocket notification for subscribed wallets
        if let Some(ref tx_sender) = self.tx_event_sender {
            let outputs: Vec<crate::rpc::websocket::TxOutputInfo> = tx
                .outputs
                .iter()
                .enumerate()
                .map(|(i, out)| {
                    let address = Self::script_pubkey_to_address(&out.script_pubkey);
                    crate::rpc::websocket::TxOutputInfo {
                        address,
                        amount: out.value as f64 / 100_000_000.0,
                        index: i as u32,
                    }
                })
                .collect();

            let event = crate::rpc::websocket::TransactionEvent {
                txid: txid_hex.clone(),
                outputs,
                timestamp: chrono::Utc::now().timestamp(),
                status: crate::rpc::websocket::TxEventStatus::Pending,
            };

            match tx_sender.send(event) {
                Ok(receivers) => {
                    tracing::info!("📡 WS tx_notification sent to {} receiver(s)", receivers);
                }
                Err(e) => {
                    tracing::warn!("📡 WS tx_notification send failed (no receivers): {}", e);
                }
            }
        }

        // For MasternodeReg transactions, immediately pre-lock the collateral UTXO so
        // that listunspentmulti returns spendable: false right away — before the tx is
        // confirmed in a block (~10 min).  This closes the double-spend window on both
        // the GUI and mobile wallets.
        if let Some(crate::types::SpecialTransactionData::MasternodeRegistration {
            ref collateral_outpoint,
            ref node_address,
            ..
        }) = tx.special_data
        {
            let parts: Vec<&str> = collateral_outpoint.split(':').collect();
            if parts.len() == 2 {
                if let (Ok(txid_bytes), Ok(vout)) = (hex::decode(parts[0]), parts[1].parse::<u32>())
                {
                    if txid_bytes.len() == 32 {
                        let mut txid_arr = [0u8; 32];
                        txid_arr.copy_from_slice(&txid_bytes);
                        let outpoint = crate::types::OutPoint {
                            txid: txid_arr,
                            vout,
                        };
                        if let Ok(utxo) = self.utxo_manager.get_utxo(&outpoint).await {
                            let mn_addr = node_address.clone();
                            let height = self.blockchain.get_height();
                            match self.utxo_manager.lock_collateral(
                                outpoint,
                                mn_addr,
                                height,
                                utxo.value,
                            ) {
                                Ok(_) => tracing::info!(
                                    "🔒 Pre-locked collateral {}:{} for pending MasternodeRegistration {}",
                                    parts[0],
                                    vout,
                                    &txid_hex[..16]
                                ),
                                Err(e) => tracing::debug!(
                                    "ℹ️ Collateral pre-lock skipped ({}): {}",
                                    &txid_hex[..16],
                                    e
                                ),
                            }
                        }
                    }
                }
            }
        }

        match self.consensus.add_transaction(tx).await {
            Ok(_) => {
                tracing::info!("✅ Transaction {} accepted by consensus", &txid_hex[..16]);
            }
            Err(e) => {
                tracing::error!(
                    "❌ Transaction {} REJECTED by consensus: {}",
                    &txid_hex[..16],
                    e
                );
                return Err(RpcError {
                    code: -26,
                    message: format!("Transaction rejected: {}", e),
                });
            }
        }

        Ok(json!(txid_hex))
    }

    async fn rebroadcast_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected txid".to_string(),
            })?;

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

        if self.consensus.rebroadcast_transaction(txid).await {
            Ok(json!({
                "txid": txid_str,
                "result": "rebroadcast"
            }))
        } else {
            Err(RpcError {
                code: -5,
                message: "Transaction not found in finalized pool (may already be archived in a block, or not yet finalized)".to_string(),
            })
        }
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
            version: 2,
            inputs: tx_inputs,
            outputs: tx_outputs,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            lock_time: 0,
            special_data: None,
            encrypted_memo: None,
        };

        // Serialize and return hex
        let tx_bytes = bincode::serialize(&tx).map_err(|_| RpcError {
            code: -32603,
            message: "Failed to serialize transaction".to_string(),
        })?;

        Ok(json!(hex::encode(tx_bytes)))
    }

    async fn get_treasury_balance(&self) -> Result<Value, RpcError> {
        let satoshis = self.blockchain.get_treasury_balance();
        Ok(json!({
            "balance": satoshis as f64 / 100_000_000.0,
            "satoshis": satoshis
        }))
    }

    async fn get_balance(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params.first().and_then(|v| v.as_str());

        let filter_addr = if let Some(addr) = address {
            addr.to_string()
        } else if let Some(local_mn) = self.registry.get_local_masternode().await {
            // Use wallet_address — the node's own spendable address.
            // reward_address may be an external GUI wallet; its UTXOs can't be signed here.
            local_mn.masternode.wallet_address
        } else if let Some(wallet_addr) = self.registry.get_local_wallet_address().await {
            // Fallback: masternode may have been deregistered but wallet address is still valid
            wallet_addr
        } else {
            return Ok(json!({
                "balance": 0.0,
                "locked": 0.0,
                "available": 0.0
            }));
        };

        let utxos = self.utxo_manager.list_utxos_by_address(&filter_addr).await;

        // Collect the masternode addresses for every node that uses this wallet.
        // An operator may run multiple masternodes (e.g. two Bronze nodes) from the
        // same wallet address — a collateral lock from any of them counts as "our" lock.
        let our_mn_addresses: std::collections::HashSet<String> = self
            .registry
            .list_all()
            .await
            .into_iter()
            .filter(|info| {
                info.masternode.wallet_address == filter_addr
                    || (!info.reward_address.is_empty() && info.reward_address == filter_addr)
            })
            .map(|info| info.masternode.address)
            .collect();

        let mut spendable: u64 = 0;
        let mut locked_collateral: u64 = 0;
        let mut pending: u64 = 0;

        for u in &utxos {
            // Only count a collateral lock as "locked" if one of our own nodes placed it.
            // Foreign masternodes can gossip any outpoint as their collateral; if one
            // of those is in our UTXO set we must not hide it from our spendable balance.
            if let Some(lock) = self.utxo_manager.get_locked_collateral(&u.outpoint) {
                if our_mn_addresses.contains(&lock.masternode_address) {
                    locked_collateral += u.value;
                    continue;
                }
            }
            match self.utxo_manager.get_state(&u.outpoint) {
                Some(crate::types::UTXOState::Unspent) => spendable += u.value,
                Some(crate::types::UTXOState::Locked { .. }) => pending += u.value,
                Some(crate::types::UTXOState::SpentPending { .. }) => {} // being spent, don't count
                Some(crate::types::UTXOState::SpentFinalized { .. }) => {} // spent, don't count
                Some(crate::types::UTXOState::Archived { .. }) => {}     // spent & archived
                None => {}                                               // unknown state
            }
        }

        let total = spendable + locked_collateral + pending;

        Ok(json!({
            "balance": total as f64 / 100_000_000.0,
            "locked": locked_collateral as f64 / 100_000_000.0,
            "available": spendable as f64 / 100_000_000.0
        }))
    }

    /// Get combined balance across multiple addresses (batch query for HD wallets)
    /// Params: [["addr1", "addr2", ...]]
    async fn get_balances(&self, params: &[Value]) -> Result<Value, RpcError> {
        let addresses = params
            .first()
            .and_then(|v| v.as_array())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected array of addresses".to_string(),
            })?;

        if addresses.len() > 1000 {
            return Err(RpcError {
                code: -32602,
                message: "Too many addresses (max 1000)".to_string(),
            });
        }

        let mut total_spendable: u64 = 0;
        let mut total_locked: u64 = 0;
        let mut total_pending: u64 = 0;
        let mut per_address = Vec::new();

        for addr_val in addresses {
            let addr = addr_val.as_str().unwrap_or("");
            if addr.is_empty() {
                continue;
            }

            let utxos = self.utxo_manager.list_utxos_by_address(addr).await;

            let mut spendable: u64 = 0;
            let mut locked: u64 = 0;
            let mut pending: u64 = 0;

            for u in &utxos {
                if self.utxo_manager.is_collateral_locked(&u.outpoint) {
                    locked += u.value;
                    continue;
                }
                match self.utxo_manager.get_state(&u.outpoint) {
                    Some(crate::types::UTXOState::Unspent) => spendable += u.value,
                    Some(crate::types::UTXOState::Locked { .. }) => pending += u.value,
                    Some(crate::types::UTXOState::SpentPending { .. }) => {} // being spent, don't count
                    Some(crate::types::UTXOState::SpentFinalized { .. }) => {} // spent, don't count
                    Some(crate::types::UTXOState::Archived { .. }) => {}     // spent & archived
                    None => {}                                               // unknown state
                }
            }

            if spendable > 0 || locked > 0 || pending > 0 {
                per_address.push(json!({
                    "address": addr,
                    "balance": (spendable + locked + pending) as f64 / 100_000_000.0,
                    "available": spendable as f64 / 100_000_000.0,
                    "locked": locked as f64 / 100_000_000.0,
                }));
            }

            total_spendable += spendable;
            total_locked += locked;
            total_pending += pending;
        }

        let total = total_spendable + total_locked + total_pending;

        Ok(json!({
            "balance": total as f64 / 100_000_000.0,
            "locked": total_locked as f64 / 100_000_000.0,
            "available": total_spendable as f64 / 100_000_000.0,
            "addresses": per_address,
            "address_count": addresses.len(),
        }))
    }

    /// List unspent outputs across multiple addresses (batch query for HD wallets)
    /// Params: [["addr1", "addr2", ...], min_conf, max_conf, limit]
    async fn list_unspent_multi(&self, params: &[Value]) -> Result<Value, RpcError> {
        let addresses = params
            .first()
            .and_then(|v| v.as_array())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected array of addresses".to_string(),
            })?;

        if addresses.len() > 1000 {
            return Err(RpcError {
                code: -32602,
                message: "Too many addresses (max 1000)".to_string(),
            });
        }

        let min_conf = params.get(1).and_then(|v| v.as_u64()).unwrap_or(0);
        let max_conf = params.get(2).and_then(|v| v.as_u64()).unwrap_or(9999999);
        let limit = params.get(3).and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let current_height = self.blockchain.get_height();
        let mut result: Vec<Value> = Vec::new();

        for addr_val in addresses {
            let addr = addr_val.as_str().unwrap_or("");
            if addr.is_empty() {
                continue;
            }

            let utxos = self.utxo_manager.list_utxos_by_address(addr).await;

            for u in &utxos {
                if limit > 0 && result.len() >= limit {
                    break;
                }

                let state = self.utxo_manager.get_state(&u.outpoint);
                let is_locked = self.utxo_manager.is_collateral_locked(&u.outpoint);

                // Skip spent/archived UTXOs
                match &state {
                    Some(crate::types::UTXOState::SpentPending { .. })
                    | Some(crate::types::UTXOState::SpentFinalized { .. })
                    | Some(crate::types::UTXOState::Archived { .. }) => continue,
                    _ => {}
                }

                let (spendable, state_str) = match state {
                    Some(crate::types::UTXOState::Unspent) if !is_locked => (true, "unspent"),
                    Some(crate::types::UTXOState::Unspent) if is_locked => {
                        (false, "collateral_locked")
                    }
                    Some(crate::types::UTXOState::Locked { .. }) => (false, "locked"),
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

                if confirmations >= min_conf && confirmations <= max_conf {
                    result.push(json!({
                        "txid": hex::encode(u.outpoint.txid),
                        "vout": u.outpoint.vout,
                        "address": u.address,
                        "amount": u.value as f64 / 100_000_000.0,
                        "confirmations": confirmations,
                        "spendable": spendable,
                        "state": state_str,
                    }));
                }
            }

            if limit > 0 && result.len() >= limit {
                break;
            }
        }

        Ok(json!(result))
    }

    async fn list_unspent(&self, params: &[Value]) -> Result<Value, RpcError> {
        // Default min_conf=0: TIME Coin has instant finality via TimeVote,
        // so finalized transaction outputs should be visible immediately
        let min_conf = params.first().and_then(|v| v.as_u64()).unwrap_or(0);
        let max_conf = params.get(1).and_then(|v| v.as_u64()).unwrap_or(9999999);
        let addresses = params.get(2).and_then(|v| v.as_array());
        let limit = params.get(3).and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        let current_height = self.blockchain.get_height();

        // Determine which addresses to query UTXOs for
        let query_addresses: Vec<String> = if let Some(addrs) = addresses {
            // Use explicitly provided addresses
            addrs
                .iter()
                .filter_map(|a| a.as_str().map(|s| s.to_string()))
                .collect()
        } else {
            // Fallback to local masternode/wallet address
            let local_address = self
                .registry
                .get_local_masternode()
                .await
                .map(|mn| mn.reward_address);
            let local_address = match local_address {
                Some(addr) => Some(addr),
                None => self.registry.get_local_wallet_address().await,
            };
            match local_address {
                Some(addr) => vec![addr],
                None => return Ok(json!([])),
            }
        };

        // Collect txids already in the on-chain UTXO set to avoid duplicates
        let mut seen_outpoints: std::collections::HashSet<(Vec<u8>, u32)> =
            std::collections::HashSet::new();

        let mut filtered: Vec<Value> = Vec::new();

        for query_addr in &query_addresses {
            let utxos = self.utxo_manager.list_utxos_by_address(query_addr).await;

            for u in &utxos {
                seen_outpoints.insert((u.outpoint.txid.to_vec(), u.outpoint.vout));

                // Get UTXO state
                let state = self.utxo_manager.get_state(&u.outpoint);
                let is_locked = self.utxo_manager.is_collateral_locked(&u.outpoint);

                // Skip spent/archived UTXOs — listunspent only returns unspent outputs
                match &state {
                    Some(crate::types::UTXOState::SpentPending { .. })
                    | Some(crate::types::UTXOState::SpentFinalized { .. })
                    | Some(crate::types::UTXOState::Archived { .. }) => continue,
                    _ => {}
                }

                let (spendable, state_str) = match state {
                    Some(crate::types::UTXOState::Unspent) if !is_locked => (true, "unspent"),
                    Some(crate::types::UTXOState::Unspent) if is_locked => {
                        (false, "collateral_locked")
                    }
                    Some(crate::types::UTXOState::Locked { .. }) => (false, "locked"),
                    None => (false, "unknown"),
                    _ => (false, "unavailable"),
                };

                let confirmations = {
                    let from_index = self
                        .blockchain
                        .tx_index
                        .as_ref()
                        .and_then(|idx| idx.get_location(&u.outpoint.txid))
                        .map(|loc| current_height.saturating_sub(loc.block_height) + 1);
                    if let Some(c) = from_index {
                        c
                    } else if u.outpoint.txid == self.blockchain.genesis_hash() {
                        // Genesis UTXOs use the genesis block hash as txid (no real tx).
                        // Genesis is at height 0, so confirmations = current_height + 1.
                        current_height + 1
                    } else {
                        0
                    }
                };

                if confirmations >= min_conf && confirmations <= max_conf {
                    filtered.push(json!({
                        "txid": hex::encode(u.outpoint.txid),
                        "vout": u.outpoint.vout,
                        "address": u.address,
                        "amount": u.value as f64 / 100_000_000.0,
                        "confirmations": confirmations,
                        "spendable": spendable,
                        "state": state_str,
                        "solvable": true,
                        "safe": true
                    }));
                }
            }
        }

        // Include outputs from finalized transactions not yet in a block.
        // TIME Coin achieves instant finality via TimeVote consensus (67% threshold),
        // so finalized transaction outputs are safe to display before block inclusion.
        if min_conf == 0 {
            let addr_set: std::collections::HashSet<&str> =
                query_addresses.iter().map(|s| s.as_str()).collect();
            let finalized_txs = self.consensus.tx_pool.get_finalized_transactions();
            for tx in &finalized_txs {
                let txid = tx.txid();
                for (vout, output) in tx.outputs.iter().enumerate() {
                    let output_address =
                        Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                    if !addr_set.contains(output_address.as_str()) {
                        continue;
                    }
                    if seen_outpoints.contains(&(txid.to_vec(), vout as u32)) {
                        continue;
                    }
                    filtered.push(json!({
                        "txid": hex::encode(txid),
                        "vout": vout,
                        "address": output_address,
                        "amount": output.value as f64 / 100_000_000.0,
                        "confirmations": 0,
                        "spendable": true,
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

        let current_height = self.blockchain.get_height();

        // Get local masternode's reward address to filter UTXOs
        let local_address = match self.registry.get_local_masternode().await {
            Some(mn) => mn.reward_address,
            None => {
                return Ok(json!([]));
            }
        };

        let utxos = self
            .utxo_manager
            .list_utxos_by_address(&local_address)
            .await;

        // Group UTXOs by address: (total_amount, tx_count, min_confirmations)
        use std::collections::HashMap;
        let mut address_map: HashMap<String, (u64, usize, u64)> = HashMap::new();

        let genesis_hash = self.blockchain.genesis_hash();
        for utxo in utxos.iter() {
            let confirmations = {
                let from_index = self
                    .blockchain
                    .tx_index
                    .as_ref()
                    .and_then(|idx| idx.get_location(&utxo.outpoint.txid))
                    .map(|loc| current_height.saturating_sub(loc.block_height) + 1);
                if let Some(c) = from_index {
                    c
                } else if utxo.outpoint.txid == genesis_hash {
                    current_height + 1
                } else {
                    0
                }
            };

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
        // params: [address, count] or [count] (legacy)
        let (local_address, count) = match params.first() {
            Some(Value::String(addr)) => {
                let count = params.get(1).and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                (addr.clone(), count)
            }
            Some(Value::Number(n)) => {
                let count = n.as_u64().unwrap_or(10) as usize;
                let addr = self
                    .registry
                    .get_local_masternode()
                    .await
                    .map(|mn| mn.reward_address)
                    .ok_or_else(|| RpcError {
                        code: -4,
                        message: "No address provided and node is not a masternode".to_string(),
                    })?;
                (addr, count)
            }
            _ => {
                let addr = self
                    .registry
                    .get_local_masternode()
                    .await
                    .map(|mn| mn.reward_address)
                    .ok_or_else(|| RpcError {
                        code: -4,
                        message: "No address provided and node is not a masternode".to_string(),
                    })?;
                (addr, 10)
            }
        };

        let chain_height = self.blockchain.get_height();
        let mut transactions: Vec<Value> = Vec::new();

        // Scan blocks from newest to oldest, collecting wallet-related TXs
        let scan_start = chain_height;
        for height in (0..=scan_start).rev() {
            if count > 0 && transactions.len() >= count {
                break;
            }

            let block = match self.blockchain.get_block(height) {
                Ok(b) => b,
                Err(_) => continue,
            };

            let block_hash = hex::encode(block.hash());
            let block_time = block.header.timestamp;

            // Genesis block has no transactions — its rewards are stored directly
            // in block.masternode_rewards and materialised as UTXOs at startup.
            // Synthesise a history entry so the wallet shows the allocation.
            if height == 0 && block.transactions.is_empty() {
                let received: u64 = block
                    .masternode_rewards
                    .iter()
                    .filter(|(addr, _)| addr == &local_address)
                    .map(|(_, amt)| amt)
                    .sum();
                if received > 0 {
                    transactions.push(json!({
                        "txid": block_hash,
                        "category": "genesis",
                        "amount": received as f64 / 100_000_000.0,
                        "confirmations": chain_height + 1,
                        "blockhash": block_hash,
                        "blockheight": 0u64,
                        "blocktime": block_time,
                        "time": block_time,
                        "memo": "Genesis Allocation",
                    }));
                }
                continue; // no transactions to iterate
            }

            for (tx_idx, tx) in block.transactions.iter().enumerate() {
                let txid = hex::encode(tx.txid());

                // Check if any output goes to our address (receive)
                let mut received: u64 = 0;
                for output in &tx.outputs {
                    let addr = Self::script_pubkey_to_address(&output.script_pubkey);
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
                                        let src_addr = Self::script_pubkey_to_address(
                                            &src_output.script_pubkey,
                                        );
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
                    // Detect consolidation: all outputs go to our address (self-send)
                    let all_outputs_to_self = sent > 0
                        && received > 0
                        && tx.outputs.iter().all(|o| {
                            Self::script_pubkey_to_address(&o.script_pubkey) == local_address
                        });

                    // Try to decrypt encrypted memo if present
                    let memo = tx
                        .encrypted_memo
                        .as_ref()
                        .and_then(|encrypted| self.consensus.decrypt_memo(encrypted));

                    // Skip coinbase (tx_idx 0) and reward distribution (tx_idx 1) for "send"
                    // They are always "receive" type
                    let category = if tx_idx <= 1 {
                        "generate"
                    } else if all_outputs_to_self {
                        "consolidate"
                    } else if sent > 0 && received > 0 {
                        // Change back to self — net effect is a send
                        "send"
                    } else if sent > 0 {
                        "send"
                    } else {
                        "receive"
                    };

                    let net_amount = if category == "consolidate" {
                        // Show the consolidated output value (what you end up with)
                        received as f64 / 100_000_000.0
                    } else if category == "send" {
                        // For sends, show the net amount leaving the wallet (negative)
                        // sent - received = total input from wallet - change back
                        -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                    } else {
                        received as f64 / 100_000_000.0
                    };

                    // Calculate fee for sends and consolidations
                    let fee = if category == "send" || category == "consolidate" {
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
                        "time": tx.timestamp,
                    });

                    if let Some(f) = fee {
                        entry["fee"] = json!(f);
                    }

                    if category == "generate" {
                        // Block reward: the encrypted_memo here belongs to the
                        // masternode, not the wallet. Always emit a plain label.
                        entry["memo"] = json!("Block Reward");
                    } else if let Some(ref m) = memo {
                        entry["memo"] = json!(m);
                    } else if let Some(ref enc) = tx.encrypted_memo {
                        entry["encrypted_memo"] = json!(hex::encode(enc));
                    }

                    transactions.push(entry);
                }
            }
        }

        // Truncate to requested count (0 = unlimited)
        if count > 0 {
            transactions.truncate(count);
        }

        // Include finalized-but-not-yet-in-block transactions from consensus pool
        let finalized_txs = self.consensus.tx_pool.get_finalized_transactions();
        let existing_txids: std::collections::HashSet<String> = transactions
            .iter()
            .filter_map(|t| {
                t.get("txid")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect();

        for tx in &finalized_txs {
            let txid = hex::encode(tx.txid());
            if existing_txids.contains(&txid) {
                continue;
            }

            let mut received: u64 = 0;
            for output in &tx.outputs {
                let addr = Self::script_pubkey_to_address(&output.script_pubkey);
                if addr == local_address {
                    received += output.value;
                }
            }

            let mut sent: u64 = 0;
            for input in &tx.inputs {
                if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                    if utxo.address == local_address {
                        sent += utxo.value;
                    }
                }
            }

            if sent > 0 || received > 0 {
                let outputs_all_to_self = received > 0
                    && tx
                        .outputs
                        .iter()
                        .all(|o| Self::script_pubkey_to_address(&o.script_pubkey) == local_address);
                let category = if sent > 0 && outputs_all_to_self {
                    "consolidate"
                } else if sent > 0 {
                    "send"
                } else {
                    "receive"
                };
                let net_amount = if category == "consolidate" {
                    received as f64 / 100_000_000.0
                } else if category == "send" {
                    -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                } else {
                    received as f64 / 100_000_000.0
                };
                let fee = if category == "send" || category == "consolidate" {
                    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                    if sent > total_out {
                        Some(-((sent - total_out) as f64 / 100_000_000.0))
                    } else {
                        None
                    }
                } else {
                    None
                };

                let memo = tx
                    .encrypted_memo
                    .as_ref()
                    .and_then(|encrypted| self.consensus.decrypt_memo(encrypted));

                let mut entry = json!({
                    "txid": txid,
                    "category": category,
                    "amount": net_amount,
                    "confirmations": 0,
                    "finalized": true,
                    "time": tx.timestamp,
                    "blocktime": tx.timestamp,
                });

                if let Some(f) = fee {
                    entry["fee"] = json!(f);
                }

                if let Some(ref m) = memo {
                    entry["memo"] = json!(m);
                } else if let Some(ref enc) = tx.encrypted_memo {
                    entry["encrypted_memo"] = json!(hex::encode(enc));
                }

                transactions.insert(0, entry);
            }
        }

        // Also include pending (not yet finalized) transactions
        let pending_txs = self.consensus.tx_pool.get_pending_transactions();
        let existing_txids: std::collections::HashSet<String> = transactions
            .iter()
            .filter_map(|t| {
                t.get("txid")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect();

        for tx in &pending_txs {
            let txid = hex::encode(tx.txid());
            if existing_txids.contains(&txid) {
                continue;
            }

            let mut received: u64 = 0;
            for output in &tx.outputs {
                let addr = Self::script_pubkey_to_address(&output.script_pubkey);
                if addr == local_address {
                    received += output.value;
                }
            }

            let mut sent: u64 = 0;
            for input in &tx.inputs {
                if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                    if utxo.address == local_address {
                        sent += utxo.value;
                    }
                }
            }

            if sent > 0 || received > 0 {
                let outputs_all_to_self = received > 0
                    && tx
                        .outputs
                        .iter()
                        .all(|o| Self::script_pubkey_to_address(&o.script_pubkey) == local_address);
                let category = if sent > 0 && outputs_all_to_self {
                    "consolidate"
                } else if sent > 0 {
                    "send"
                } else {
                    "receive"
                };
                let net_amount = if category == "consolidate" {
                    received as f64 / 100_000_000.0
                } else if category == "send" {
                    -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                } else {
                    received as f64 / 100_000_000.0
                };
                let fee = if category == "send" || category == "consolidate" {
                    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                    if sent > total_out {
                        Some(-((sent - total_out) as f64 / 100_000_000.0))
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
                    "confirmations": 0,
                    "finalized": false,
                    "time": tx.timestamp,
                    "blocktime": tx.timestamp,
                });
                if let Some(f) = fee {
                    entry["fee"] = json!(f);
                }
                if let Some(ref enc) = tx.encrypted_memo {
                    entry["encrypted_memo"] = json!(hex::encode(enc));
                }
                transactions.insert(0, entry);
            }
        }

        Ok(json!(transactions))
    }

    /// List transactions across multiple addresses (batch query for HD wallets)
    /// Params: [["addr1", "addr2", ...], count]
    async fn list_transactions_multi(&self, params: &[Value]) -> Result<Value, RpcError> {
        let addresses = params
            .first()
            .and_then(|v| v.as_array())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected array of addresses".to_string(),
            })?;

        if addresses.len() > 1000 {
            return Err(RpcError {
                code: -32602,
                message: "Too many addresses (max 1000)".to_string(),
            });
        }

        let count = params.get(1).and_then(|v| v.as_u64()).unwrap_or(1000) as usize;
        // Optional: only scan blocks >= from_height (default 0 = full history).
        // Enables incremental polling: the wallet passes its last-known block
        // height so only new blocks are scanned on subsequent polls.
        let from_height = params.get(2).and_then(|v| v.as_u64()).unwrap_or(0);

        // Build a set of addresses for fast lookup
        let addr_set: std::collections::HashSet<String> = addresses
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        if addr_set.is_empty() {
            return Ok(json!({"transactions": [], "chain_height": self.blockchain.get_height()}));
        }

        let chain_height = self.blockchain.get_height();
        let mut transactions: Vec<Value> = Vec::new();

        for height in (from_height..=chain_height).rev() {
            if count > 0 && transactions.len() >= count {
                break;
            }

            let block = match self.blockchain.get_block(height) {
                Ok(b) => b,
                Err(_) => continue,
            };

            let block_hash = hex::encode(block.hash());
            let block_time = block.header.timestamp;

            // Genesis block stores rewards directly in masternode_rewards (no transactions)
            if height == 0 && block.transactions.is_empty() {
                for (addr, amount) in &block.masternode_rewards {
                    if addr_set.contains(addr) && *amount > 0 {
                        transactions.push(json!({
                            "txid": block_hash,
                            "address": addr,
                            "category": "genesis",
                            "amount": *amount as f64 / 100_000_000.0,
                            "confirmations": chain_height + 1,
                            "blockhash": block_hash,
                            "blockheight": 0u64,
                            "blocktime": block_time,
                            "time": block_time,
                            "memo": "Genesis Allocation",
                        }));
                    }
                }
                continue;
            }

            for (tx_idx, tx) in block.transactions.iter().enumerate() {
                let txid = hex::encode(tx.txid());

                // Check outputs: track wallet receives and the first external recipient
                let mut received: u64 = 0;
                let mut recv_address = String::new();
                let mut recv_vout: u32 = 0;
                let mut ext_address = String::new(); // first non-wallet output = real recipient
                let mut ext_vout: u32 = 0;
                for (vout_idx, output) in tx.outputs.iter().enumerate() {
                    let addr = Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                    if addr_set.contains(&addr) {
                        received += output.value;
                        if recv_address.is_empty() {
                            recv_address = addr;
                            recv_vout = vout_idx as u32;
                        }
                    } else if ext_address.is_empty() {
                        ext_address = addr;
                        ext_vout = vout_idx as u32;
                    }
                }

                // Check inputs for any of our addresses.
                // Track whether any inputs could NOT be resolved (tx_index miss)
                // so we can fall back to output-only consolidation detection below.
                let mut sent: u64 = 0;
                let mut send_address = String::new();
                let mut unresolved_inputs = 0usize;
                for input in &tx.inputs {
                    let spent_txid = input.previous_output.txid;
                    let spent_vout = input.previous_output.vout;

                    let resolved = if let Some(ref txi) = self.blockchain.tx_index {
                        if let Some(loc) = txi.get_location(&spent_txid) {
                            if let Ok(src_block) = self.blockchain.get_block(loc.block_height) {
                                if let Some(src_tx) = src_block.transactions.get(loc.tx_index) {
                                    if let Some(src_output) =
                                        src_tx.outputs.get(spent_vout as usize)
                                    {
                                        let src_addr = Self::script_pubkey_to_address(
                                            &src_output.script_pubkey,
                                        )
                                        .to_string();
                                        if addr_set.contains(&src_addr) {
                                            sent += src_output.value;
                                            if send_address.is_empty() {
                                                send_address = src_addr;
                                            }
                                        }
                                        true // resolved (even if not ours)
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false // tx_index has no entry for this input's source txid
                        }
                    } else {
                        false // no tx_index at all
                    };
                    if !resolved {
                        unresolved_inputs += 1;
                    }
                }

                if sent > 0 || received > 0 {
                    // For coinbase/reward-distribution transactions (tx_idx 0 or 1),
                    // the wallet address may appear only as a staking *input* with no
                    // corresponding output in that same tx (the payout arrives later in
                    // another "generate" entry where received > 0).  Skip these to avoid
                    // flooding the wallet with +0.00 TIME "generate" entries.
                    if tx_idx <= 1 && received == 0 {
                        continue;
                    }

                    // Detect consolidation: all outputs go to one of our tracked addresses.
                    // The primary signal is sent > 0 (inputs resolved via tx_index).
                    // Fallback: if ALL inputs were unresolvable (tx_index miss after resync)
                    // but ALL outputs land on wallet addresses, treat as consolidation to avoid
                    // incorrectly reporting the receive as income.
                    let outputs_all_to_self = received > 0
                        && tx.outputs.iter().all(|o| {
                            let addr = Self::script_pubkey_to_address(&o.script_pubkey).to_string();
                            addr_set.contains(&addr)
                        });
                    let all_outputs_to_self = sent > 0 && outputs_all_to_self;
                    let likely_consolidation_no_index = sent == 0
                        && unresolved_inputs > 0
                        && unresolved_inputs == tx.inputs.len()
                        && outputs_all_to_self;

                    let category = if tx_idx <= 1 {
                        "generate"
                    } else if all_outputs_to_self || likely_consolidation_no_index {
                        "consolidate"
                    } else if sent > 0 {
                        "send"
                    } else {
                        "receive"
                    };

                    let net_amount = if category == "consolidate" {
                        received as f64 / 100_000_000.0
                    } else if category == "send" {
                        -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                    } else {
                        received as f64 / 100_000_000.0
                    };

                    let fee = if category == "send" || category == "consolidate" {
                        let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                        if sent > total_out {
                            Some(-((sent - total_out) as f64 / 100_000_000.0))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    let (address, vout) = if category == "send" && !ext_address.is_empty() {
                        (&ext_address, ext_vout)
                    } else if !recv_address.is_empty() {
                        (&recv_address, recv_vout)
                    } else {
                        (&send_address, 0u32)
                    };

                    let mut entry = json!({
                        "txid": txid,
                        "address": address,
                        "vout": vout,
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

                    let memo = tx
                        .encrypted_memo
                        .as_ref()
                        .and_then(|encrypted| self.consensus.decrypt_memo(encrypted));
                    if category == "generate" {
                        entry["memo"] = json!("Block Reward");
                    } else if let Some(ref m) = memo {
                        entry["memo"] = json!(m);
                    } else if let Some(ref enc) = tx.encrypted_memo {
                        entry["encrypted_memo"] = json!(hex::encode(enc));
                    }

                    transactions.push(entry);
                }
            }
        }

        if count > 0 {
            transactions.truncate(count);
        }

        // Include finalized-but-not-yet-in-block transactions from consensus pool
        let finalized_txs = self.consensus.tx_pool.get_finalized_transactions();
        let existing_txids: std::collections::HashSet<String> = transactions
            .iter()
            .filter_map(|t| {
                t.get("txid")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect();

        for tx in &finalized_txs {
            let txid = hex::encode(tx.txid());
            if existing_txids.contains(&txid) {
                continue;
            }

            let mut received: u64 = 0;
            let mut recv_address = String::new();
            let mut recv_vout: u32 = 0;
            let mut ext_address = String::new();
            let mut ext_vout: u32 = 0;
            for (vout_idx, output) in tx.outputs.iter().enumerate() {
                let addr = Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                if addr_set.contains(&addr) {
                    received += output.value;
                    if recv_address.is_empty() {
                        recv_address = addr;
                        recv_vout = vout_idx as u32;
                    }
                } else if ext_address.is_empty() {
                    ext_address = addr;
                    ext_vout = vout_idx as u32;
                }
            }

            let mut sent: u64 = 0;
            let mut send_address = String::new();
            for input in &tx.inputs {
                if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                    let src_addr = utxo.address.clone();
                    if addr_set.contains(&src_addr) {
                        sent += utxo.value;
                        if send_address.is_empty() {
                            send_address = src_addr;
                        }
                    }
                }
            }

            if sent > 0 || received > 0 {
                let outputs_all_to_self = received > 0
                    && tx.outputs.iter().all(|o| {
                        let addr = Self::script_pubkey_to_address(&o.script_pubkey).to_string();
                        addr_set.contains(&addr)
                    });
                let category = if sent > 0 && outputs_all_to_self {
                    "consolidate"
                } else if sent > 0 {
                    "send"
                } else {
                    "receive"
                };
                let net_amount = if category == "consolidate" {
                    received as f64 / 100_000_000.0
                } else if category == "send" {
                    -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                } else {
                    received as f64 / 100_000_000.0
                };
                let fee = if category == "send" || category == "consolidate" {
                    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                    if sent > total_out {
                        Some(-((sent - total_out) as f64 / 100_000_000.0))
                    } else {
                        None
                    }
                } else {
                    None
                };

                let (address, vout) = if category == "send" && !ext_address.is_empty() {
                    (&ext_address, ext_vout)
                } else if !recv_address.is_empty() {
                    (&recv_address, recv_vout)
                } else {
                    (&send_address, 0u32)
                };

                let mut entry = json!({
                    "txid": txid,
                    "address": address,
                    "vout": vout,
                    "category": category,
                    "amount": net_amount,
                    "confirmations": 0,
                    "finalized": true,
                    "time": tx.timestamp,
                    "blocktime": tx.timestamp,
                });

                if let Some(f) = fee {
                    entry["fee"] = json!(f);
                }

                let memo = tx
                    .encrypted_memo
                    .as_ref()
                    .and_then(|encrypted| self.consensus.decrypt_memo(encrypted));
                if let Some(ref m) = memo {
                    entry["memo"] = json!(m);
                } else if let Some(ref enc) = tx.encrypted_memo {
                    entry["encrypted_memo"] = json!(hex::encode(enc));
                }

                transactions.insert(0, entry);
            }
        }

        // Also include pending (not yet finalized) transactions
        let pending_txs = self.consensus.tx_pool.get_pending_transactions();
        let existing_txids: std::collections::HashSet<String> = transactions
            .iter()
            .filter_map(|t| {
                t.get("txid")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect();

        for tx in &pending_txs {
            let txid = hex::encode(tx.txid());
            if existing_txids.contains(&txid) {
                continue;
            }

            let mut received: u64 = 0;
            let mut recv_address = String::new();
            let mut recv_vout: u32 = 0;
            let mut ext_address = String::new();
            let mut ext_vout: u32 = 0;
            for (vout_idx, output) in tx.outputs.iter().enumerate() {
                let addr = Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                if addr_set.contains(&addr) {
                    received += output.value;
                    if recv_address.is_empty() {
                        recv_address = addr;
                        recv_vout = vout_idx as u32;
                    }
                } else if ext_address.is_empty() {
                    ext_address = addr;
                    ext_vout = vout_idx as u32;
                }
            }

            let mut sent: u64 = 0;
            let mut send_address = String::new();
            for input in &tx.inputs {
                if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                    let src_addr = utxo.address.clone();
                    if addr_set.contains(&src_addr) {
                        sent += utxo.value;
                        if send_address.is_empty() {
                            send_address = src_addr;
                        }
                    }
                }
            }

            if sent > 0 || received > 0 {
                let outputs_all_to_self = received > 0
                    && tx.outputs.iter().all(|o| {
                        let addr = Self::script_pubkey_to_address(&o.script_pubkey).to_string();
                        addr_set.contains(&addr)
                    });
                let category = if sent > 0 && outputs_all_to_self {
                    "consolidate"
                } else if sent > 0 {
                    "send"
                } else {
                    "receive"
                };
                let net_amount = if category == "consolidate" {
                    received as f64 / 100_000_000.0
                } else if category == "send" {
                    -((sent.saturating_sub(received)) as f64 / 100_000_000.0)
                } else {
                    received as f64 / 100_000_000.0
                };
                let fee = if category == "send" || category == "consolidate" {
                    let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                    if sent > total_out {
                        Some(-((sent - total_out) as f64 / 100_000_000.0))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let (address, vout) = if category == "send" && !ext_address.is_empty() {
                    (&ext_address, ext_vout)
                } else if !recv_address.is_empty() {
                    (&recv_address, recv_vout)
                } else {
                    (&send_address, 0u32)
                };
                let mut entry = json!({
                    "txid": txid,
                    "address": address,
                    "vout": vout,
                    "category": category,
                    "amount": net_amount,
                    "confirmations": 0,
                    "finalized": false,
                    "time": tx.timestamp,
                    "blocktime": tx.timestamp,
                });

                if let Some(f) = fee {
                    entry["fee"] = json!(f);
                }

                let memo = tx
                    .encrypted_memo
                    .as_ref()
                    .and_then(|encrypted| self.consensus.decrypt_memo(encrypted));
                if let Some(ref m) = memo {
                    entry["memo"] = json!(m);
                } else if let Some(ref enc) = tx.encrypted_memo {
                    entry["encrypted_memo"] = json!(hex::encode(enc));
                }

                transactions.insert(0, entry);
            }
        }

        Ok(json!({
            "transactions": transactions,
            "chain_height": chain_height,
        }))
    }

    /// Remove a masternode from the in-memory registry and persistent sled storage.
    /// Takes one parameter: the `address` field (IP:port) as shown in `masternodelist`.
    /// The node will not re-appear unless it reconnects and re-announces.
    async fn masternode_remove(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected: masternoderemove \"<ip:port>\"".to_string(),
            })?;

        match self.registry.unregister(address).await {
            Ok(Some(info)) => {
                tracing::info!(
                    "RPC masternoderemove: removed {} ({})",
                    address,
                    info.masternode.wallet_address
                );
                Ok(serde_json::json!({
                    "result": "removed",
                    "address": address,
                    "wallet": info.masternode.wallet_address,
                }))
            }
            Ok(None) => Ok(serde_json::json!({
                "result": "not_found",
                "address": address,
            })),
            Err(e) => Err(RpcError {
                code: -1,
                message: format!("Registry error: {}", e),
            }),
        }
    }

    async fn masternode_status(&self) -> Result<Value, RpcError> {
        if let Some(local_mn) = self.registry.get_local_masternode().await {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let computed_uptime = if local_mn.is_active && local_mn.uptime_start > 0 {
                local_mn.total_uptime + now_secs.saturating_sub(local_mn.uptime_start)
            } else {
                local_mn.total_uptime
            };

            let current_height = self.blockchain.get_height();
            let is_free = matches!(local_mn.masternode.tier, crate::types::MasternodeTier::Free);

            // No maturity gate — all on-chain registered nodes are immediately eligible.
            let is_mature = true;
            let blocks_until_mature = 0u64;

            // Reachability grace: newly-registered nodes get 5 min before probe is enforced
            let in_grace_period = local_mn.first_seen_at > 0
                && now_secs.saturating_sub(local_mn.first_seen_at)
                    < crate::masternode_registry::REACHABILITY_GRACE_PERIOD_SECS;

            // Human-readable diagnosis of why rewards may not be arriving
            let eligibility_status = if !is_free {
                if local_mn.is_active {
                    "eligible".to_string()
                } else {
                    "inactive — not seen by enough peers".to_string()
                }
            } else if !is_mature {
                format!(
                    "maturing — {} blocks remaining (~{} min until first reward eligible)",
                    blocks_until_mature,
                    blocks_until_mature * 10
                )
            } else if in_grace_period {
                "reachability probe pending — waiting for inbound TCP probe on port 24000"
                    .to_string()
            } else if !local_mn.is_publicly_reachable {
                "ineligible — port 24000 not publicly reachable; check firewall / port forwarding"
                    .to_string()
            } else {
                "eligible — competing for Free pool rewards".to_string()
            };

            Ok(json!({
                "status": "active",
                "address": local_mn.masternode.address,
                "reward_address": local_mn.reward_address,
                "tier": format!("{:?}", local_mn.masternode.tier),
                "uptime_start": local_mn.uptime_start,
                "total_uptime": computed_uptime,
                "is_active": local_mn.is_active,
                "is_publicly_reachable": local_mn.is_publicly_reachable,
                "registration_height": local_mn.registration_height,
                "current_height": current_height,
                "blocks_since_registration": current_height.saturating_sub(local_mn.registration_height),
                "maturity_required_blocks": 0,
                "blocks_until_mature": blocks_until_mature,
                "is_mature": is_mature,
                "in_grace_period": in_grace_period,
                "eligibility_status": eligibility_status,
                "last_reward_height": local_mn.last_reward_height,
                "blocks_without_reward": local_mn.blocks_without_reward,
                "public_key": hex::encode(local_mn.masternode.public_key.to_bytes()),
                "version": env!("CARGO_PKG_VERSION"),
                "git_hash": option_env!("GIT_HASH").unwrap_or("unknown")
            }))
        } else {
            Ok(json!({
                "status": "not_a_masternode",
                "message": "This node is not configured as a masternode (set masternode=1 in time.conf)"
            }))
        }
    }

    /// Check the health of the local node's configured collateral UTXO(s).
    ///
    /// Reports:
    /// - Whether the UTXO exists on-chain
    /// - The detected tier (Bronze/Silver/Gold) based on value
    /// - Whether it is locked as collateral in UTXOManager
    /// - Whether a squatter (different IP) has claimed it in the gossip registry
    async fn check_collateral(&self) -> Result<Value, RpcError> {
        let local_mn = match self.registry.get_local_masternode().await {
            Some(mn) => mn,
            None => {
                return Ok(json!({
                    "status": "not_configured",
                    "message": "This node is not configured as a masternode (masternode=1 not set)"
                }));
            }
        };

        let outpoint = match local_mn.masternode.collateral_outpoint {
            Some(ref op) => op.clone(),
            None => {
                return Ok(json!({
                    "status": "no_collateral",
                    "tier": format!("{:?}", local_mn.masternode.tier),
                    "message": "Free-tier node — no collateral configured in masternode.conf"
                }));
            }
        };

        let txid_hex = hex::encode(outpoint.txid);
        let outpoint_str = format!("{}:{}", txid_hex, outpoint.vout);

        // UTXO on-chain existence check
        let utxo_result = self.utxo_manager.get_utxo(&outpoint).await;
        let (utxo_found, utxo_value, utxo_address, detected_tier) = match &utxo_result {
            Ok(utxo) => {
                let tier = crate::types::MasternodeTier::from_collateral_value(utxo.value)
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|| format!("Unknown ({} TIME)", utxo.value / 100_000_000));
                (true, utxo.value, utxo.address.clone(), tier)
            }
            Err(_) => (false, 0u64, String::new(), String::from("Unknown")),
        };

        // UTXOManager lock check
        let lock_info = self.utxo_manager.get_locked_collateral(&outpoint);
        let is_locked = lock_info.is_some();
        let locked_to = lock_info
            .as_ref()
            .map(|l| l.masternode_address.clone())
            .unwrap_or_default();

        // Registry squatter check
        let registry_ip = self
            .registry
            .get_registered_ip_for_collateral(&outpoint)
            .await;
        let local_ip = local_mn.masternode.address.clone();
        let (squatter, squatter_ip) = match &registry_ip {
            Some(ip) if ip != &local_ip => (true, ip.clone()),
            _ => (false, String::new()),
        };

        let tier_match = !squatter
            && (format!("{:?}", local_mn.masternode.tier) == detected_tier
                || local_mn.masternode.tier == crate::types::MasternodeTier::Free);

        let overall = if !utxo_found {
            "utxo_not_found"
        } else if squatter {
            "squatted"
        } else if !is_locked {
            "not_locked"
        } else {
            "ok"
        };

        Ok(json!({
            "status": overall,
            "collateral": outpoint_str,
            "txid": txid_hex,
            "vout": outpoint.vout,
            "utxo_found": utxo_found,
            "utxo_value_satoshis": utxo_value,
            "utxo_value_time": utxo_value / 100_000_000,
            "utxo_address": utxo_address,
            "detected_tier": detected_tier,
            "configured_tier": format!("{:?}", local_mn.masternode.tier),
            "tier_match": tier_match,
            "collateral_locked": is_locked,
            "locked_to_ip": locked_to,
            "registry_ip": registry_ip.unwrap_or_else(|| local_ip.clone()),
            "local_ip": local_ip,
            "squatter_detected": squatter,
            "squatter_ip": squatter_ip,
            "advice": if !utxo_found {
                "UTXO not found — send 1000/10000/100000 TIME to yourself and add txid:vout to masternode.conf, or add tier=bronze/silver/gold to time.conf to skip UTXO lookup"
            } else if squatter {
                "Squatter detected — restart both nodes (v1.4.34+) to trigger V4 auto-eviction, or use 'masternodereg' to force an on-chain reclaim"
            } else if !is_locked {
                "UTXO found but not locked — restart timed to re-register collateral lock"
            } else {
                "Collateral is healthy"
            }
        }))
    }

    /// Look up who (if anyone) is currently claiming an arbitrary collateral outpoint.
    ///
    /// Unlike `checkcollateral` (which only inspects the locally-configured collateral),
    /// this works for any txid:vout — useful for diagnosing squatted UTXOs that are
    /// no longer configured in masternode.conf.
    async fn find_collateral(&self, params: &[Value]) -> Result<Value, RpcError> {
        let outpoint_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Usage: findcollateral <txid:vout>".to_string(),
            })?;

        // Accept both "txid:vout" and separate txid + vout params
        let (txid_str, vout_u32) = if let Some(pos) = outpoint_str.find(':') {
            let txid = &outpoint_str[..pos];
            let vout: u32 = outpoint_str[pos + 1..].parse().map_err(|_| RpcError {
                code: -32602,
                message: "Invalid vout in outpoint string".to_string(),
            })?;
            (txid.to_string(), vout)
        } else {
            let vout_val = params
                .get(1)
                .and_then(|v| v.as_u64())
                .ok_or_else(|| RpcError {
                    code: -32602,
                    message: "Usage: findcollateral <txid:vout> OR findcollateral <txid> <vout>"
                        .to_string(),
                })? as u32;
            (outpoint_str.to_string(), vout_val)
        };

        let txid_bytes = hex::decode(&txid_str).map_err(|_| RpcError {
            code: -8,
            message: "Invalid txid hex".to_string(),
        })?;
        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -8,
                message: "txid must be 32 bytes (64 hex chars)".to_string(),
            });
        }
        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);
        let outpoint = crate::types::OutPoint {
            txid,
            vout: vout_u32,
        };
        let outpoint_display = format!("{}:{}", txid_str, vout_u32);

        // Registry: who currently claims this outpoint via gossip?
        let registry_ip = self
            .registry
            .get_registered_ip_for_collateral(&outpoint)
            .await;

        // UTXOManager collateral lock
        let lock_info = self.utxo_manager.get_locked_collateral(&outpoint);
        let is_locked = lock_info.is_some();
        let locked_to = lock_info
            .as_ref()
            .map(|l| l.masternode_address.clone())
            .unwrap_or_default();

        // On-chain UTXO
        let utxo_result = self.utxo_manager.get_utxo(&outpoint).await;
        let (utxo_found, utxo_value, utxo_address, detected_tier) = match &utxo_result {
            Ok(utxo) => {
                let tier = crate::types::MasternodeTier::from_collateral_value(utxo.value)
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|| format!("Unknown ({} TIME)", utxo.value / 100_000_000));
                (true, utxo.value, utxo.address.clone(), tier)
            }
            Err(_) => (false, 0u64, String::new(), "Unknown".to_string()),
        };

        // Sled canonical anchor
        let anchor_ip = self.registry.get_collateral_anchor(&outpoint);

        let claimant = registry_ip.clone().or_else(|| anchor_ip.clone());
        let status = if claimant.is_none() {
            "unclaimed"
        } else {
            "claimed"
        };

        Ok(json!({
            "outpoint": outpoint_display,
            "status": status,
            "registry_claimant": registry_ip,
            "anchor_claimant": anchor_ip,
            "collateral_locked": is_locked,
            "locked_to": locked_to,
            "utxo_found": utxo_found,
            "utxo_value_satoshis": utxo_value,
            "utxo_value_time": utxo_value / 100_000_000,
            "utxo_address": utxo_address,
            "detected_tier": detected_tier,
            "advice": if !utxo_found {
                "UTXO not found on this node — chain may not have it yet, or it was spent"
            } else if claimant.is_some() {
                "Outpoint is claimed — if this is your collateral, run 'masternodereg' with the correct IP to reclaim it on-chain, or configure it in masternode.conf and restart to trigger V4 auto-eviction"
            } else {
                "Outpoint is not claimed by any node"
            }
        }))
    }

    async fn masternode_genkey(&self) -> Result<Value, RpcError> {
        let mut seed = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut seed);
        let key = crate::masternode_certificate::encode_masternode_key(&seed);
        Ok(json!(key))
    }

    async fn get_fee_schedule(&self) -> Result<Value, RpcError> {
        let schedule = crate::consensus::FeeSchedule::default();
        let tiers: Vec<Value> = schedule
            .tiers
            .iter()
            .map(|(up_to, bps)| {
                json!({
                    "up_to": up_to,
                    "rate_bps": bps,
                })
            })
            .collect();
        Ok(json!({
            "tiers": tiers,
            "min_fee": schedule.min_fee,
        }))
    }

    /// Returns collateral requirements per masternode tier.
    async fn masternode_reg_info(&self) -> Result<Value, RpcError> {
        use crate::types::MasternodeTier;
        Ok(json!({
            "tiers": {
                "Bronze": {
                    "collateral": MasternodeTier::Bronze.collateral(),
                    "collateral_time": MasternodeTier::Bronze.collateral() / 100_000_000,
                },
                "Silver": {
                    "collateral": MasternodeTier::Silver.collateral(),
                    "collateral_time": MasternodeTier::Silver.collateral() / 100_000_000,
                },
                "Gold": {
                    "collateral": MasternodeTier::Gold.collateral(),
                    "collateral_time": MasternodeTier::Gold.collateral() / 100_000_000,
                },
            },
            "note": "Free tier masternodes register via handshake (no collateral required)"
        }))
    }

    /// Check registration status for a masternode registration transaction.
    async fn masternode_reg_status(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_hex = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected txid".to_string(),
            })?;

        let txid_bytes = hex::decode(txid_hex).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid txid hex".to_string(),
        })?;
        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "txid must be 32 bytes".to_string(),
            });
        }
        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);

        // Check if the transaction exists in the block index
        if let Some(ref tx_index) = self.blockchain.tx_index {
            if let Some(loc) = tx_index.get_location(&txid) {
                return Ok(json!({
                    "txid": txid_hex,
                    "status": "confirmed",
                    "block_height": loc.block_height,
                    "tx_index": loc.tx_index,
                }));
            }
        }

        // Check mempool
        if self.consensus.tx_pool.get_transaction(&txid).is_some() {
            return Ok(json!({
                "txid": txid_hex,
                "status": "pending",
                "message": "Transaction is in the mempool, awaiting block inclusion"
            }));
        }

        Ok(json!({
            "txid": txid_hex,
            "status": "not_found",
            "message": "Transaction not found in blocks or mempool"
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

    /// Return the Ed25519 public key for a TIME address (if known).
    /// The pubkey is learned when the address signs a transaction on-chain.
    async fn get_address_pubkey(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected address".to_string(),
            })?;

        let pubkey_hex = self
            .consensus
            .utxo_manager
            .find_pubkey_for_address(address)
            .map(hex::encode)
            .unwrap_or_default();

        Ok(json!({
            "address": address,
            "pubkey": pubkey_hex,
        }))
    }

    /// Pre-register an Ed25519 public key for a TIME address.
    ///
    /// Wallets call this at startup so the node can encrypt memos to them
    /// even before they appear as a sender in any on-chain transaction.
    ///
    /// Params: [address (string), pubkey_hex (64-char hex string)]
    ///
    /// Validation: derives the TIME address from the supplied pubkey and
    /// checks it matches the claimed address, preventing fake registrations.
    async fn register_address_pubkey(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected [address, pubkey_hex]".to_string(),
            })?;

        let pubkey_hex = params
            .get(1)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected [address, pubkey_hex]".to_string(),
            })?;

        let pubkey_bytes = hex::decode(pubkey_hex).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid pubkey_hex: not valid hex".to_string(),
        })?;

        if pubkey_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: format!(
                    "Invalid pubkey_hex: expected 32 bytes (64 hex chars), got {}",
                    pubkey_bytes.len()
                ),
            });
        }

        // Derive the network from the address prefix so we can validate.
        let network = if address.starts_with("TIME1") {
            NetworkType::Mainnet
        } else if address.starts_with("TIME0") {
            NetworkType::Testnet
        } else {
            return Err(RpcError {
                code: -32602,
                message: "Invalid address: must start with TIME0 (testnet) or TIME1 (mainnet)"
                    .to_string(),
            });
        };

        // Derive the address from the supplied pubkey and verify it matches.
        let derived = Address::from_public_key(&pubkey_bytes, network).to_string();
        if derived != address {
            return Err(RpcError {
                code: -5,
                message: format!(
                    "Pubkey does not match address: derived {} but got {}",
                    derived, address
                ),
            });
        }

        let mut arr = [0u8; 32];
        arr.copy_from_slice(&pubkey_bytes);

        self.consensus.utxo_manager.register_pubkey(address, arr);

        tracing::debug!("📬 Registered pubkey for address {}", address);

        Ok(json!({ "success": true, "address": address }))
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

    async fn get_raw_mempool(&self, params: &[Value]) -> Result<Value, RpcError> {
        let verbose = params.first().and_then(|v| v.as_bool()).unwrap_or(false);
        if verbose {
            return self.get_mempool_verbose().await;
        }

        // Non-verbose: return array of txids (Bitcoin default behavior)
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

    async fn get_mempool_verbose(&self) -> Result<Value, RpcError> {
        let entries = self.consensus.tx_pool.get_all_entries_verbose();
        let txs: Vec<Value> = entries
            .iter()
            .map(|(tx, fee, age_secs, status)| {
                let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
                let first_output_addr = tx
                    .outputs
                    .first()
                    .map(|o| Self::script_pubkey_to_address(&o.script_pubkey).to_string())
                    .unwrap_or_default();
                let size = bincode::serialize(tx).map(|b| b.len()).unwrap_or(0);
                let vin: Vec<Value> = tx
                    .inputs
                    .iter()
                    .map(|inp| {
                        json!({
                            "txid": hex::encode(inp.previous_output.txid),
                            "vout": inp.previous_output.vout,
                        })
                    })
                    .collect();
                let vout: Vec<Value> = tx
                    .outputs
                    .iter()
                    .enumerate()
                    .map(|(n, out)| {
                        json!({
                            "value": out.value as f64 / 100_000_000.0,
                            "n": n,
                            "address": Self::script_pubkey_to_address(&out.script_pubkey),
                        })
                    })
                    .collect();
                json!({
                    "txid": hex::encode(tx.txid()),
                    "status": status,
                    "fee": *fee,
                    "fee_time": *fee as f64 / 100_000_000.0,
                    "amount": output_sum as f64 / 100_000_000.0,
                    "amount_sats": output_sum,
                    "size": size,
                    "inputs": tx.inputs.len(),
                    "outputs": tx.outputs.len(),
                    "age_secs": age_secs,
                    "to": first_output_addr,
                    "vin": vin,
                    "vout": vout,
                })
            })
            .collect();
        Ok(json!(txs))
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

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Filter to connected only if show_all is false
        let filtered_list: Vec<Value> = full_list
            .iter()
            .filter(|(_, is_connected, _, _)| show_all || *is_connected)
            .map(
                |(mn, is_connected, collateral_locked, collateral_outpoint)| {
                    let computed_uptime = if mn.is_active && mn.uptime_start > 0 {
                        mn.total_uptime + now_secs.saturating_sub(mn.uptime_start)
                    } else {
                        mn.total_uptime
                    };
                    json!({
                        "address": mn.masternode.address,
                        "wallet_address": mn.masternode.wallet_address,
                        "collateral": mn.masternode.collateral as f64 / 100_000_000.0,
                        "tier": format!("{:?}", mn.masternode.tier),
                        "registered_at": mn.masternode.registered_at,
                        "is_active": mn.is_active,
                        "is_connected": is_connected,
                        "is_publicly_reachable": mn.is_publicly_reachable,
                        "uptime_start": mn.uptime_start,
                        "total_uptime": computed_uptime,
                        "daemon_started_at": mn.daemon_started_at,
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
        // sendtoaddress "to_address" amount [subtract_fee] [nowait] [memo]
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

        let subtract_fee = params.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
        let nowait = params.get(3).and_then(|v| v.as_bool()).unwrap_or(false);
        let memo = params.get(4).and_then(|v| v.as_str());

        // Use the node's own wallet address (the one whose private key lives on
        // this server).  reward_address may point to an external GUI wallet that
        // this node cannot sign for, so we must never use it as the spend source.
        let wallet_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.masternode.wallet_address)
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode - no wallet address".to_string(),
            })?;

        self.send_coins(
            &wallet_address,
            to_address,
            amount,
            subtract_fee,
            nowait,
            memo,
        )
        .await
    }

    async fn send_from(&self, params: &[Value]) -> Result<Value, RpcError> {
        // sendfrom "from_address" "to_address" amount [subtract_fee] [nowait] [memo]
        let from_address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected from_address".to_string(),
            })?;

        let to_address = params
            .get(1)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected to_address".to_string(),
            })?;

        let amount = params
            .get(2)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected amount".to_string(),
            })?;

        let subtract_fee = params.get(3).and_then(|v| v.as_bool()).unwrap_or(false);
        let nowait = params.get(4).and_then(|v| v.as_bool()).unwrap_or(false);
        let memo = params.get(5).and_then(|v| v.as_str());

        self.send_coins(from_address, to_address, amount, subtract_fee, nowait, memo)
            .await
    }

    async fn send_coins(
        &self,
        from_address: &str,
        to_address: &str,
        amount: f64,
        subtract_fee: bool,
        nowait: bool,
        memo: Option<&str>,
    ) -> Result<Value, RpcError> {
        // Maximum inputs per transaction (~9000 would hit 1MB TX size limit;
        // cap lower to leave headroom and prevent excessive memory use)
        const MAX_TX_INPUTS: usize = 5000;

        // Convert TIME to smallest unit (like satoshis)
        let amount_units = (amount * 100_000_000.0) as u64;

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

            // Get UTXOs for the source address (fresh each attempt)
            let wallet_utxos = self.utxo_manager.list_utxos_by_address(from_address).await;

            // Filter: unspent, not collateral, not in exclusion set
            let mut utxos: Vec<_> = wallet_utxos
                .into_iter()
                .filter(|u| {
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
            utxos.sort_by_key(|b| std::cmp::Reverse(b.value));

            // Calculate fee using governance-adjustable tiered schedule
            let fee_schedule = crate::consensus::FeeSchedule::default();
            let fee = fee_schedule.required_fee(amount_units);

            // Select sufficient UTXOs
            let mut selected_utxos = Vec::new();
            let mut total_input = 0u64;
            for utxo in &utxos {
                if selected_utxos.len() >= MAX_TX_INPUTS {
                    break;
                }
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

            // Check if we hit the input limit before gathering enough funds — auto-consolidate
            if selected_utxos.len() >= MAX_TX_INPUTS && total_input < amount_units + fee {
                // Auto-consolidate: merge up to MAX_TX_INPUTS smallest UTXOs into one
                tracing::info!(
                    "🔄 Auto-consolidating {} UTXOs (need more inputs for {} TIME transfer)",
                    MAX_TX_INPUTS,
                    amount / 100_000_000.0
                );

                // Take the smallest UTXOs for consolidation (they're sorted desc, so take from end)
                let consolidate_count = MAX_TX_INPUTS.min(utxos.len());
                let mut consolidate_utxos: Vec<_> = utxos
                    .iter()
                    .rev()
                    .take(consolidate_count)
                    .cloned()
                    .collect();
                // But cap at MAX_TX_INPUTS
                consolidate_utxos.truncate(MAX_TX_INPUTS);

                let consolidate_total: u64 = consolidate_utxos.iter().map(|u| u.value).sum();
                // Self-sends (consolidations) only pay MIN_TX_FEE, not 1% of total value
                let consolidate_fee = crate::consensus::MIN_TX_FEE;

                if consolidate_total <= consolidate_fee {
                    return Err(RpcError {
                        code: -6,
                        message: "UTXOs too small to consolidate — total value less than fee"
                            .to_string(),
                    });
                }

                let cons_inputs: Vec<TxInput> = consolidate_utxos
                    .iter()
                    .map(|utxo| TxInput {
                        previous_output: utxo.outpoint.clone(),
                        script_sig: vec![],
                        sequence: 0xFFFFFFFF,
                    })
                    .collect();

                let cons_outputs = vec![TxOutput {
                    value: consolidate_total - consolidate_fee,
                    script_pubkey: from_address.as_bytes().to_vec(),
                }];

                // Encrypt consolidation memo
                let consolidation_memo = self
                    .consensus
                    .encrypt_memo_for_self("UTXO Consolidation")
                    .ok();

                let mut cons_tx = Transaction {
                    version: 2,
                    inputs: cons_inputs,
                    outputs: cons_outputs,
                    lock_time: 0,
                    timestamp: chrono::Utc::now().timestamp(),
                    special_data: None,
                    encrypted_memo: consolidation_memo,
                };

                self.consensus
                    .sign_transaction(&mut cons_tx)
                    .map_err(|e| RpcError {
                        code: -4,
                        message: format!("Failed to sign consolidation transaction: {}", e),
                    })?;

                let cons_txid = cons_tx.txid();
                let cons_txid_hex = hex::encode(cons_txid);

                match self.consensus.submit_transaction(cons_tx).await {
                    Ok(_) => {
                        tracing::info!(
                            "✅ Consolidation TX {} submitted ({} UTXOs → 1, {} TIME)",
                            &cons_txid_hex[..16],
                            consolidate_utxos.len(),
                            (consolidate_total - consolidate_fee) / 100_000_000
                        );

                        // Wait for consolidation to finalize before retrying the original send
                        let timeout = Duration::from_secs(30);
                        let start = tokio::time::Instant::now();
                        while start.elapsed() < timeout {
                            if self.consensus.tx_pool.is_finalized(&cons_txid) {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }

                        if !self.consensus.tx_pool.is_finalized(&cons_txid) {
                            return Err(RpcError {
                                code: -26,
                                message: format!(
                                    "Consolidation TX {} submitted but not finalized within 30s. \
                                     Retry your send after it confirms.",
                                    cons_txid_hex
                                ),
                            });
                        }

                        tracing::info!(
                            "✅ Consolidation TX {} finalized — retrying original send",
                            &cons_txid_hex[..16]
                        );
                        // Reset exclusions and retry with the newly consolidated UTXO
                        excluded.clear();
                        last_error = "auto-consolidation".to_string();
                        continue;
                    }
                    Err(e) => {
                        return Err(RpcError {
                            code: -26,
                            message: format!(
                                "Transaction requires too many inputs ({} UTXOs). \
                                 Auto-consolidation failed: {}. \
                                 Try sending a smaller amount.",
                                selected_utxos.len(),
                                e
                            ),
                        });
                    }
                }
            }

            let send_amount = if subtract_fee {
                if total_input < amount_units {
                    return Err(RpcError {
                        code: -6,
                        message: "Insufficient funds".to_string(),
                    });
                }
                let fee = fee_schedule.required_fee(amount_units);
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
                    script_pubkey: from_address.as_bytes().to_vec(),
                });
            }

            // Encrypt memo if provided
            let encrypted_memo = if let Some(memo_text) = memo {
                // Get recipient's Ed25519 pubkey from their address
                Some(
                    self.consensus
                        .encrypt_memo_for_address(memo_text, to_address)
                        .map_err(|e| RpcError {
                            code: -4,
                            message: format!(
                                "Failed to encrypt memo: {}. \
                                 The recipient must have at least one on-chain transaction \
                                 visible to this node, or use `paypaymentrequest` which \
                                 includes the recipient pubkey in the URI.",
                                e
                            ),
                        })?,
                )
            } else {
                None
            };

            let mut tx = Transaction {
                version: 2,
                inputs,
                outputs,
                lock_time: 0,
                timestamp: chrono::Utc::now().timestamp(),
                special_data: None,
                encrypted_memo,
            };

            // Sign all inputs with wallet key
            self.consensus
                .sign_transaction(&mut tx)
                .map_err(|e| RpcError {
                    code: -4,
                    message: format!("Failed to sign transaction: {}", e),
                })?;

            let txid = tx.txid();

            // Build WS output info before tx is consumed by submit
            let ws_outputs: Vec<crate::rpc::websocket::TxOutputInfo> = tx
                .outputs
                .iter()
                .enumerate()
                .map(|(i, out)| {
                    let address = Self::script_pubkey_to_address(&out.script_pubkey);
                    crate::rpc::websocket::TxOutputInfo {
                        address,
                        amount: out.value as f64 / 100_000_000.0,
                        index: i as u32,
                    }
                })
                .collect();

            match self.consensus.submit_transaction(tx).await {
                Ok(_) => {
                    let txid_hex = hex::encode(txid);

                    // Emit pending WS notification immediately so wallets see "Pending"
                    if let Some(ref tx_sender) = self.tx_event_sender {
                        let event = crate::rpc::websocket::TransactionEvent {
                            txid: txid_hex.clone(),
                            outputs: ws_outputs.clone(),
                            timestamp: chrono::Utc::now().timestamp(),
                            status: crate::rpc::websocket::TxEventStatus::Pending,
                        };
                        match tx_sender.send(event) {
                            Ok(n) => tracing::info!(
                                "📡 WS tx_notification (pending) sent to {} receiver(s)",
                                n
                            ),
                            Err(e) => tracing::warn!("📡 WS tx_notification send failed: {}", e),
                        }
                    }

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

                            // Emit declined WS notification so wallets see "Declined"
                            if let Some(ref tx_sender) = self.tx_event_sender {
                                let event = crate::rpc::websocket::TransactionEvent {
                                    txid: txid_hex.clone(),
                                    outputs: ws_outputs.clone(),
                                    timestamp: chrono::Utc::now().timestamp(),
                                    status: crate::rpc::websocket::TxEventStatus::Declined(
                                        reason.clone(),
                                    ),
                                };
                                let _ = tx_sender.send(event);
                            }

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

        // Get UTXOs filtered by address using the address index
        let mut utxos = if let Some(addr) = filter_address {
            self.utxo_manager.list_utxos_by_address(addr).await
        } else {
            self.utxo_manager
                .list_utxos_by_address(&local_address)
                .await
        };

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
                script_sig: vec![],
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

        // Encrypt consolidation memo
        let merge_memo = self.consensus.encrypt_memo_for_self("UTXO Merge").ok();

        let mut tx = Transaction {
            version: 2,
            inputs,
            outputs,
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
            special_data: None,
            encrypted_memo: merge_memo,
        };

        // Sign all inputs with wallet key
        self.consensus
            .sign_transaction(&mut tx)
            .map_err(|e| RpcError {
                code: -4,
                message: format!("Failed to sign transaction: {}", e),
            })?;

        let txid = tx.txid();

        // Broadcast transaction to consensus engine
        match self.consensus.process_transaction(tx, None).await {
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
            "info": "Whitelisted IPs are exempt from rate limiting and bans. Only peers verified via time-coin.io may be whitelisted. Use 'addwhitelist <ip>' to add (requires network peer verification)."
        }))
    }

    /// Add IP to whitelist — only permitted if the IP appears in the official
    /// peer list published at `https://time-coin.io/api/peers` (or the testnet
    /// equivalent). This prevents arbitrary IPs from being whitelisted; only
    /// registered network masternodes/peers can bypass rate-limiting and bans.
    async fn add_whitelist(&self, params: &[Value]) -> Result<Value, RpcError> {
        let ip_str = params.first().and_then(|v| v.as_str()).ok_or(RpcError {
            code: -32602,
            message: "IP address parameter required".to_string(),
        })?;

        let ip_addr = ip_str.parse::<std::net::IpAddr>().map_err(|_| RpcError {
            code: -32602,
            message: format!("Invalid IP address: {}", ip_str),
        })?;

        // ── Authorization check ─────────────────────────────────────────────
        // Fetch the official peer list and confirm this IP is on it before
        // we whitelist it.  Fail closed: if the API is unreachable we refuse
        // the request rather than silently allowing unknown IPs through.
        let peers_url = self.network.peer_discovery_url();
        let known_ips = fetch_official_peer_ips(peers_url)
            .await
            .map_err(|e| RpcError {
                code: -1,
                message: format!(
                    "Cannot verify peer — official peer list unavailable ({}). Try again later.",
                    e
                ),
            })?;

        if !known_ips.contains(&ip_addr) {
            return Err(RpcError {
                code: -8,
                message: format!(
                    "{} is not listed in the official peer registry ({}). \
                     Only registered network peers may be whitelisted.",
                    ip_str, peers_url
                ),
            });
        }
        // ────────────────────────────────────────────────────────────────────

        let mut bl = self.blacklist.write().await;
        if bl.is_whitelisted(ip_addr) {
            Ok(json!({
                "result": "already_whitelisted",
                "ip": ip_str,
                "message": "IP is already whitelisted"
            }))
        } else {
            bl.add_to_whitelist(
                ip_addr,
                "Added via RPC (verified against official peer list)",
            );
            Ok(json!({
                "result": "success",
                "ip": ip_str,
                "message": format!("IP added to whitelist (verified via {})", peers_url)
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
        let (permanent, temporary, subnets, violations) = bl.list_bans();
        let (perm_count, temp_count, viol_count, wl_count) = bl.stats();
        let subnet_count = subnets.len();

        let permanent_list: Vec<Value> = permanent
            .into_iter()
            .map(|(ip, reason)| json!({"ip": ip, "reason": reason}))
            .collect();

        let temporary_list: Vec<Value> = temporary
            .into_iter()
            .map(|(ip, secs, reason)| json!({"ip": ip, "remaining_secs": secs, "reason": reason}))
            .collect();

        let subnet_list: Vec<Value> = subnets
            .into_iter()
            .map(|(cidr, reason)| json!({"subnet": cidr, "reason": reason}))
            .collect();

        let violations_list: Vec<Value> = {
            let mut v = violations;
            v.sort_by_key(|b| std::cmp::Reverse(b.1));
            v.into_iter()
                .map(|(ip, count)| json!({"ip": ip, "violations": count}))
                .collect()
        };

        Ok(json!({
            "summary": {
                "permanent_bans": perm_count,
                "temporary_bans": temp_count,
                "subnet_bans": subnet_count,
                "active_violations": viol_count,
                "whitelisted": wl_count
            },
            "permanent": permanent_list,
            "temporary": temporary_list,
            "subnets": subnet_list,
            "violations": violations_list
        }))
    }

    async fn ban_ip(&self, params: &[Value]) -> Result<Value, RpcError> {
        let ip_str = params.first().and_then(|v| v.as_str()).ok_or(RpcError {
            code: -32602,
            message: "IP address parameter required".to_string(),
        })?;

        let ip_addr = ip_str.parse::<std::net::IpAddr>().map_err(|_| RpcError {
            code: -32602,
            message: format!("Invalid IP address: {}", ip_str),
        })?;

        let reason = params
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("manual ban via RPC");

        let mut bl = self.blacklist.write().await;
        bl.add_permanent_ban(ip_addr, reason);
        tracing::info!(
            "🔨 RPC: Permanently banned {} (reason: {})",
            ip_addr,
            reason
        );
        Ok(json!({
            "result": "success",
            "ip": ip_str,
            "message": format!("IP {} permanently banned", ip_str)
        }))
    }

    /// Run the collateral squatter audit sweep on demand.
    ///
    /// Scans every paid-tier registered masternode, verifies that its
    /// `reward_address` matches the on-chain collateral UTXO output address,
    /// and detects duplicate outpoint claims.  Confirmed squatters are evicted,
    /// their collateral lock released, and permanently banned.
    ///
    /// Returns a summary of findings: squatters evicted, locks released, bans issued.
    async fn audit_collateral(&self) -> Result<Value, RpcError> {
        use std::collections::HashMap;

        let all_nodes = self.registry.get_all().await;
        let mut evicted: Vec<Value> = Vec::new();
        let mut warnings: Vec<Value> = Vec::new();

        // Pass 1: reward_address vs UTXO address mismatch
        let mut evicted_ips: Vec<String> = Vec::new();
        for info in &all_nodes {
            if info.masternode.tier == crate::types::MasternodeTier::Free {
                continue;
            }
            let outpoint = match &info.masternode.collateral_outpoint {
                Some(op) => op.clone(),
                None => continue,
            };
            let utxo = match self.utxo_manager.get_utxo(&outpoint).await {
                Ok(u) => u,
                Err(_) => continue,
            };
            if utxo.address.is_empty() {
                continue;
            }
            let ip = info.masternode.address.clone();
            if info.reward_address != utxo.address {
                let _ = self.registry.unregister(&ip).await;
                let _ = self.utxo_manager.unlock_collateral(&outpoint);
                let bare = ip.split(':').next().unwrap_or(&ip);
                if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                    let mut bl = self.blacklist.write().await;
                    bl.add_permanent_ban(
                        ban_ip,
                        "collateral squatter: reward_address ≠ UTXO owner (manual audit)",
                    );
                }
                evicted_ips.push(ip.clone());
                evicted.push(json!({
                    "ip": ip,
                    "outpoint": outpoint.to_string(),
                    "reason": "reward_address does not match UTXO owner address",
                    "registered_reward_address": info.reward_address,
                    "utxo_owner_address": utxo.address,
                    "action": "evicted, lock released, permanently banned"
                }));
            }
        }

        // Pass 2: duplicate outpoint detection
        let mut outpoint_map: HashMap<String, Vec<crate::masternode_registry::MasternodeInfo>> =
            HashMap::new();
        for info in &all_nodes {
            if info.masternode.tier == crate::types::MasternodeTier::Free {
                continue;
            }
            if let Some(ref op) = info.masternode.collateral_outpoint {
                let key = format!("{}:{}", hex::encode(op.txid), op.vout);
                outpoint_map.entry(key).or_default().push(info.clone());
            }
        }
        for (outpoint_str, claimants) in &outpoint_map {
            if claimants.len() < 2 {
                continue;
            }
            let outpoint = match claimants
                .first()
                .and_then(|c| c.masternode.collateral_outpoint.clone())
            {
                Some(op) => op,
                None => continue,
            };
            let utxo_addr = match self.utxo_manager.get_utxo(&outpoint).await {
                Ok(u) if !u.address.is_empty() => u.address,
                _ => continue,
            };
            let mut owner: Option<String> = None;
            let mut squatters: Vec<String> = Vec::new();
            let mut stalemate = false;
            for c in claimants {
                if c.reward_address == utxo_addr {
                    if owner.is_none() {
                        owner = Some(c.masternode.address.clone());
                    } else {
                        stalemate = true;
                        warnings.push(json!({
                            "outpoint": outpoint_str,
                            "claimants": claimants.iter().map(|c| &c.masternode.address).collect::<Vec<_>>(),
                            "warning": "address-match stalemate — V4 cryptographic proof required to resolve"
                        }));
                        break;
                    }
                } else {
                    squatters.push(c.masternode.address.clone());
                }
            }
            if !stalemate && owner.is_some() {
                for sq_ip in &squatters {
                    if evicted_ips.contains(sq_ip) {
                        continue;
                    }
                    let _ = self.registry.unregister(sq_ip).await;
                    let _ = self.utxo_manager.unlock_collateral(&outpoint);
                    let bare = sq_ip.split(':').next().unwrap_or(sq_ip.as_str());
                    if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                        let mut bl = self.blacklist.write().await;
                        bl.add_permanent_ban(
                            ban_ip,
                            "collateral squatter: duplicate outpoint claim (manual audit)",
                        );
                    }
                    evicted_ips.push(sq_ip.clone());
                    evicted.push(json!({
                        "ip": sq_ip,
                        "outpoint": outpoint_str,
                        "reason": "duplicate outpoint claim — legitimate owner has matching reward_address",
                        "legitimate_owner": owner,
                        "action": "evicted, lock released, permanently banned"
                    }));
                }
            }
        }

        tracing::info!(
            "🔍 RPC auditcollateral: {} squatter(s) evicted, {} warning(s)",
            evicted.len(),
            warnings.len()
        );

        Ok(json!({
            "squatters_evicted": evicted.len(),
            "warnings": warnings.len(),
            "evicted": evicted,
            "unresolved_warnings": warnings
        }))
    }

    async fn unban(&self, params: &[Value]) -> Result<Value, RpcError> {
        let ip_str = params.first().and_then(|v| v.as_str()).ok_or(RpcError {
            code: -32602,
            message: "IP address parameter required".to_string(),
        })?;

        let ip_addr = ip_str.parse::<std::net::IpAddr>().map_err(|_| RpcError {
            code: -32602,
            message: format!("Invalid IP address: {}", ip_str),
        })?;

        let mut bl = self.blacklist.write().await;
        let was_banned = bl.unban(ip_addr);

        if was_banned {
            tracing::info!("🔓 RPC: Unbanned {}", ip_addr);
            Ok(
                json!({"result": "success", "ip": ip_str, "message": "IP removed from ban list and violations cleared"}),
            )
        } else {
            Ok(
                json!({"result": "not_banned", "ip": ip_str, "message": "IP was not in the ban list (violations cleared anyway)"}),
            )
        }
    }

    async fn unban_subnet(&self, params: &[Value]) -> Result<Value, RpcError> {
        let subnet = params.first().and_then(|v| v.as_str()).ok_or(RpcError {
            code: -32602,
            message: "Subnet CIDR parameter required".to_string(),
        })?;

        let mut bl = self.blacklist.write().await;
        let removed = bl.unban_subnet(subnet);

        if removed {
            tracing::info!("🔓 RPC: Unbanned subnet {}", subnet);
            Ok(json!({
                "result": "success",
                "subnet": subnet,
                "message": "Subnet removed from ban list"
            }))
        } else {
            Ok(json!({
                "result": "not_banned",
                "subnet": subnet,
                "message": "Subnet was not in the ban list"
            }))
        }
    }

    /// Delete the sled `collateral_anchor:{txid}:{vout}` entry for an outpoint
    /// and clear any blacklist entries banned for "Collateral hijack attempt
    /// for <that outpoint>".  Used to recover from a stuck anchor that points
    /// at the wrong IP (e.g. set by a stale relay before AV36 hardening), so
    /// the legitimate owner's next V4 announce can re-anchor cleanly.
    ///
    /// Params: [outpoint] where outpoint = "<txid_hex>:<vout>"
    async fn clear_collateral_anchor(&self, params: &[Value]) -> Result<Value, RpcError> {
        let outpoint_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Usage: clearcollateralanchor <txid_hex>:<vout>".to_string(),
            })?;

        let (txid_hex, vout_str) = outpoint_str.split_once(':').ok_or_else(|| RpcError {
            code: -32602,
            message: "Outpoint must be formatted <txid_hex>:<vout>".to_string(),
        })?;
        let vout: u32 = vout_str.parse().map_err(|_| RpcError {
            code: -32602,
            message: "vout must be a non-negative integer".to_string(),
        })?;
        let txid_bytes = hex::decode(txid_hex).map_err(|_| RpcError {
            code: -32602,
            message: "txid must be hex".to_string(),
        })?;
        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "txid must be 32 bytes".to_string(),
            });
        }
        let mut txid_arr = [0u8; 32];
        txid_arr.copy_from_slice(&txid_bytes);
        let outpoint = crate::types::OutPoint {
            txid: txid_arr,
            vout,
        };

        let removed_anchor = self.registry.delete_collateral_anchor(&outpoint);

        // Auto-unban any IP previously banned with a reason mentioning this
        // outpoint, so the legitimate owner can reconnect immediately.
        let unbanned: Vec<String> = {
            let mut bl = self.blacklist.write().await;
            let needle_a = format!("Collateral hijack attempt for {}", outpoint_str);
            let needle_b = format!(
                "Collateral already locked under different anchor for {}",
                outpoint_str
            );
            let (permanent, _temp, _subnets, _viol) = bl.list_bans();
            let to_unban: Vec<std::net::IpAddr> = permanent
                .into_iter()
                .filter(|(_, reason)| reason.contains(&needle_a) || reason.contains(&needle_b))
                .filter_map(|(ip, _)| ip.parse().ok())
                .collect();
            let mut cleared = Vec::new();
            for ip in to_unban {
                if bl.unban(ip) {
                    cleared.push(ip.to_string());
                }
            }
            cleared
        };

        tracing::warn!(
            "🛠️ RPC clearcollateralanchor: outpoint={} anchor_removed={} unbanned={:?}",
            outpoint_str,
            removed_anchor,
            unbanned
        );

        Ok(json!({
            "result": "success",
            "outpoint": outpoint_str,
            "anchor_removed": removed_anchor,
            "unbanned_ips": unbanned,
            "message": format!(
                "Anchor {} for {}; cleared {} related ban(s)",
                if removed_anchor { "removed" } else { "did not exist" },
                outpoint_str,
                unbanned.len()
            )
        }))
    }

    /// Remove ALL bans and violation counts. Whitelisted peers are unaffected.
    async fn clear_ban_list(&self) -> Result<Value, RpcError> {
        let mut bl = self.blacklist.write().await;
        let (perm_count, temp_count, subnet_count, viol_count) = bl.clear_all_bans();

        tracing::info!(
            "🔓 RPC clearbanlist: cleared {} permanent ban(s), {} temporary ban(s), {} subnet ban(s), {} violation record(s)",
            perm_count, temp_count, subnet_count, viol_count
        );
        Ok(json!({
            "result": "success",
            "permanent_cleared": perm_count,
            "temporary_cleared": temp_count,
            "subnets_cleared": subnet_count,
            "violations_cleared": viol_count,
        }))
    }

    async fn reset_peer_profiles(&self) -> Result<Value, RpcError> {
        match &self.reconnection_ai {
            Some(ai) => {
                let count = ai.reset_all_profiles();
                tracing::info!("🔄 RPC: reset {} AI reconnection profile(s)", count);
                Ok(json!({
                    "result": "success",
                    "profiles_cleared": count,
                    "message": "All peer reconnection profiles cleared — peers will be retried immediately"
                }))
            }
            None => Err(RpcError {
                code: -32603,
                message: "Reconnection AI not available".to_string(),
            }),
        }
    }

    /// Delete all blocks above height 0, reset chain height to 0, and clear UTXOs.
    /// The genesis block is preserved. The node will rebuild from block 1 via peer sync.
    async fn rollback_to_block0(&self) -> Result<Value, RpcError> {
        let height_before = self.blockchain.get_height();
        if height_before == 0 {
            return Ok(json!({
                "result": "already at genesis",
                "height": 0,
                "blocks_removed": 0,
            }));
        }
        tracing::warn!(
            "⚠️  RPC rollbacktoblock0: reverting chain from height {} to genesis",
            height_before
        );
        self.blockchain.revert_to_after_genesis().await;
        Ok(json!({
            "result": "success",
            "height_before": height_before,
            "height_after": 0,
            "blocks_removed": height_before,
        }))
    }

    async fn rollback_to_height_rpc(&self, params: &[Value]) -> Result<Value, RpcError> {
        let target_height = params
            .first()
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "rollbacktoheight requires a block height argument (u64)".to_string(),
            })?;

        let height_before = self.blockchain.get_height();
        if target_height >= height_before {
            return Ok(json!({
                "result": "nothing to do",
                "height": height_before,
                "blocks_removed": 0,
            }));
        }

        tracing::warn!(
            "⚠️  RPC rollbacktoheight: reverting chain from height {} to {}",
            height_before,
            target_height
        );

        match self.blockchain.rollback_to_height(target_height).await {
            Ok(new_height) => Ok(json!({
                "result": "success",
                "height_before": height_before,
                "height_after": new_height,
                "blocks_removed": height_before - new_height,
            })),
            Err(e) => Err(RpcError {
                code: -32000,
                message: e,
            }),
        }
    }

    /// Deep fork recovery: roll back to a trusted peer's chain.
    /// Optional parameter: target height (u64). Defaults to 0 (full genesis reset).
    /// Requires at least one whitelisted peer to be connected.
    async fn resync_from_whitelist_rpc(&self, params: &[Value]) -> Result<Value, RpcError> {
        let target_height: Option<u64> = params.first().and_then(|v| v.as_u64());
        // Treat explicit 0 as "genesis reset" (same as omitting the argument)
        let target_height = target_height.and_then(|h| if h == 0 { None } else { Some(h) });

        let height_before = self.blockchain.get_height();

        tracing::warn!(
            "⚠️  RPC resyncfromwhitelist: deep fork recovery from height {} (target: {})",
            height_before,
            target_height
                .map(|h| h.to_string())
                .unwrap_or_else(|| "genesis".to_string())
        );

        match self
            .blockchain
            .resync_from_whitelisted_peers(target_height)
            .await
        {
            Ok(report) => Ok(json!({
                "result": "success",
                "height_before": report.our_height_before,
                "height_after": report.our_height_after,
                "blocks_removed": report.our_height_before.saturating_sub(report.our_height_after),
                "whitelisted_peers_used": report.peers_found,
                "best_peer": report.best_peer,
                "best_peer_height": report.best_peer_height,
                "cleared_incompatible": report.cleared_incompatible,
                "note": "Chain rolled back. Normal sync will now re-download from whitelisted peers."
            })),
            Err(e) => Err(RpcError {
                code: -32000,
                message: e,
            }),
        }
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

            // Filter by wallet_address — the address whose private key is on this
            // server.  reward_address may be an external GUI wallet; showing those
            // UTXOs would make the balance look non-zero while being unspendable.
            let spendable_addr = &local_mn.masternode.wallet_address;

            // Categorize UTXOs by state
            let mut spendable_balance: u64 = 0;
            let mut locked_collateral: u64 = 0;
            let mut pending_balance: u64 = 0;
            let mut utxo_count: usize = 0;

            // The local node's IP address, used to identify which collateral locks belong to us.
            let local_mn_addr = &local_mn.masternode.address;

            for u in utxos.iter().filter(|u| &u.address == spendable_addr) {
                utxo_count += 1;

                // Only count a collateral lock as "our locked collateral" if the lock was placed
                // by our own masternode. Foreign masternodes can claim any outpoint as their
                // collateral via gossip; if one of those outpoints happens to be in our UTXO
                // set, we must NOT show it as locked balance — we still own and can spend it.
                if let Some(lock) = self.utxo_manager.get_locked_collateral(&u.outpoint) {
                    if &lock.masternode_address == local_mn_addr {
                        locked_collateral += u.value;
                        continue;
                    }
                    // Foreign lock on our UTXO — treat as spendable (we can override via
                    // unlockcollateral RPC if needed, but don't hide it from the balance)
                }

                match self.utxo_manager.get_state(&u.outpoint) {
                    Some(crate::types::UTXOState::Unspent) => {
                        spendable_balance += u.value;
                    }
                    Some(crate::types::UTXOState::Locked { .. }) => {
                        pending_balance += u.value;
                    }
                    Some(crate::types::UTXOState::SpentPending { .. }) => {} // being spent, don't count
                    Some(crate::types::UTXOState::SpentFinalized { .. }) => {} // spent, don't count
                    Some(crate::types::UTXOState::Archived { .. }) => {}     // spent & archived
                    None => {}                                               // unknown state
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
            // Only return locks whose UTXO is still Unspent — filters out stale gossip
            // locks for spent UTXOs before the background sweep has had a chance to run.
            .filter(|lc| {
                matches!(
                    self.utxo_manager.get_state(&lc.outpoint),
                    Some(crate::types::UTXOState::Unspent)
                )
            })
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

    /// Force-unlock a masternode collateral UTXO that is stuck in the locked_collaterals map.
    /// Use this when a UTXO shows as "locked" in the dashboard but `listlockedutxos` shows nothing
    /// (the two mechanisms are separate: UTXOState::Locked vs collateral locks).
    /// Parameters: [txid, vout]
    async fn unlock_collateral_rpc(&self, params: &[Value]) -> Result<Value, RpcError> {
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

        match self.utxo_manager.unlock_collateral(&outpoint) {
            Ok(()) => Ok(json!({
                "unlocked": true,
                "txid": txid_str,
                "vout": vout,
                "message": "Collateral UTXO unlocked successfully"
            })),
            Err(crate::utxo_manager::UtxoError::NotFound) => Err(RpcError {
                code: -8,
                message: "UTXO is not collateral-locked (not found in collateral lock map). Use listlockedcollaterals to see what is locked.".to_string(),
            }),
            Err(e) => Err(RpcError {
                code: -8,
                message: format!("Failed to unlock collateral: {:?}", e),
            }),
        }
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

        // Step 1.5: Re-apply finalized-but-unarchived transactions.
        // reindex_utxos wipes everything and only restores UTXOs from blocks.
        // Transactions finalized by TimeVote but not yet in a block have their
        // output UTXOs added to the confirmed set at finalization time (in consensus.rs).
        // We must restore those outputs here and re-tombstone their inputs so the
        // UTXO set reflects the true post-finalization state.
        {
            let utxo_mgr = &self.consensus.utxo_manager;
            let finalized_txs = self.consensus.tx_pool.get_finalized_transactions();

            if !finalized_txs.is_empty() {
                tracing::info!(
                    "🔄 Re-applying {} finalized-but-unarchived transaction(s) after reindex",
                    finalized_txs.len()
                );

                // Pass 1: add output UTXOs for all finalized transactions.
                // Done before tombstoning inputs so chains within the finalized pool
                // (tx A produces an output that tx B consumes) are handled correctly.
                for tx in &finalized_txs {
                    let txid = tx.txid();
                    for (idx, output) in tx.outputs.iter().enumerate() {
                        let outpoint = crate::types::OutPoint {
                            txid,
                            vout: idx as u32,
                        };
                        let address =
                            Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                        let utxo = crate::types::UTXO {
                            outpoint: outpoint.clone(),
                            value: output.value,
                            script_pubkey: output.script_pubkey.clone(),
                            address,
                            masternode_key: None,
                        };
                        if let Err(e) = utxo_mgr.add_utxo(utxo).await {
                            tracing::debug!(
                                "Skipped re-adding finalized UTXO {}:{}: {:?}",
                                hex::encode(txid),
                                idx,
                                e
                            );
                        }
                    }
                }

                // Pass 2: tombstone all inputs of finalized transactions so they
                // are removed from the live UTXO set and sled.
                for tx in &finalized_txs {
                    let txid = tx.txid();
                    for input in &tx.inputs {
                        utxo_mgr
                            .mark_timevote_finalized(&input.previous_output, txid)
                            .await;
                    }
                }

                tracing::info!("✅ Finalized transactions re-applied after reindex");
            }

            // Re-lock inputs of pending (not yet finalized) transactions so they
            // don't appear as double-spendable while the node is running.
            let pending_txs = self.consensus.tx_pool.get_pending_transactions();
            if !pending_txs.is_empty() {
                let mut relocked = 0usize;
                for tx in &pending_txs {
                    let txid = tx.txid();
                    for input in &tx.inputs {
                        if utxo_mgr.lock_utxo(&input.previous_output, txid).is_ok() {
                            relocked += 1;
                        }
                    }
                }
                if relocked > 0 {
                    tracing::info!(
                        "🔒 Re-locked {} pending transaction input(s) after reindex",
                        relocked
                    );
                }
            }
        }

        self.restore_collateral_locks_after_reindex(height).await;

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

    async fn restore_collateral_locks_after_reindex(&self, lock_height: u64) {
        let local_address = self.registry.get_local_address().await;

        if let Some(local_mn) = self.registry.get_local_masternode().await {
            if local_mn.masternode.tier != crate::types::MasternodeTier::Free {
                if let Some(outpoint) = local_mn.masternode.collateral_outpoint.clone() {
                    match self.utxo_manager.lock_local_collateral(
                        outpoint.clone(),
                        local_mn.masternode.address.clone(),
                        lock_height,
                        local_mn.masternode.tier.collateral(),
                    ) {
                        Ok(()) => {
                            tracing::info!(
                                "🔒 Restored local collateral lock after reindex for {}:{}",
                                hex::encode(outpoint.txid),
                                outpoint.vout
                            );
                        }
                        Err(crate::utxo_manager::UtxoError::LockedAsCollateral) => {}
                        Err(e) => {
                            tracing::warn!(
                                "⚠️ Failed to restore local collateral lock after reindex for {}:{}: {:?}",
                                hex::encode(outpoint.txid),
                                outpoint.vout,
                                e
                            );
                        }
                    }
                    self.utxo_manager
                        .save_local_collateral_outpoint(Some(&outpoint));
                } else {
                    self.utxo_manager.save_local_collateral_outpoint(None);
                }
            } else {
                self.utxo_manager.save_local_collateral_outpoint(None);
            }
        } else {
            self.utxo_manager.save_local_collateral_outpoint(None);
        }

        let entries: Vec<_> = self
            .registry
            .list_all()
            .await
            .into_iter()
            .filter(|info| local_address.as_deref() != Some(info.masternode.address.as_str()))
            .filter_map(|info| {
                info.masternode
                    .collateral_outpoint
                    .as_ref()
                    .map(|outpoint| {
                        (
                            outpoint.clone(),
                            info.masternode.address.clone(),
                            lock_height,
                            info.masternode.tier.collateral(),
                        )
                    })
            })
            .collect();

        if !entries.is_empty() {
            self.utxo_manager.rebuild_collateral_locks(entries);
        }

        let purged = self.utxo_manager.purge_stale_collateral_locks();
        if purged > 0 {
            tracing::warn!(
                "🧹 [REINDEX] Purged {} stale collateral lock(s) after restore",
                purged
            );
        }
    }

    /// Reset the BFT finality lock to a lower height so a stuck node can reorg
    /// back to the canonical chain.
    ///
    /// Usage: `resetfinalitylock <height>`
    ///
    /// The target height must be strictly below the current confirmed height.
    /// After the reset the node's fork-resolution loop will automatically
    /// detect the longer canonical chain from peers and reorg to it.
    async fn reset_finality_lock_rpc(&self, params: &[Value]) -> Result<Value, RpcError> {
        let target_height = params
            .first()
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -1,
                message: "Usage: resetfinalitylock <height>".to_string(),
            })?;

        let current_confirmed = self.blockchain.get_confirmed_height();

        match self.blockchain.reset_finality_lock(target_height) {
            Ok(()) => Ok(json!({
                "status": "ok",
                "previous_confirmed_height": current_confirmed,
                "new_confirmed_height": target_height,
                "message": format!(
                    "Finality lock reset from {} to {}. \
                     The node will now accept reorgs back to height {}. \
                     Fork resolution will rerun automatically.",
                    current_confirmed, target_height, target_height
                )
            })),
            Err(e) => Err(RpcError {
                code: -8,
                message: e,
            }),
        }
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
        let mut locked: Vec<Value> = Vec::new();

        // UTXOState::Locked — in-flight transaction locks
        for (outpoint, txid, locked_at) in self.utxo_manager.get_locked_utxos() {
            if let Ok(utxo) = self.utxo_manager.get_utxo(&outpoint).await {
                let age_seconds = now - locked_at;
                locked.push(json!({
                    "txid": hex::encode(outpoint.txid),
                    "vout": outpoint.vout,
                    "address": utxo.address,
                    "amount": utxo.value as f64 / 100_000_000.0,
                    "lock_type": "transaction",
                    "locked_by_tx": hex::encode(txid),
                    "locked_at": locked_at,
                    "age_seconds": age_seconds,
                    "expired": age_seconds > 600
                }));
            } else {
                let age_seconds = now - locked_at;
                locked.push(json!({
                    "txid": hex::encode(outpoint.txid),
                    "vout": outpoint.vout,
                    "address": "Unknown (orphaned state)",
                    "amount": 0.0,
                    "lock_type": "transaction",
                    "locked_by_tx": hex::encode(txid),
                    "locked_at": locked_at,
                    "age_seconds": age_seconds,
                    "expired": age_seconds > 600,
                    "orphaned": true
                }));
            }
        }

        // Collateral locks — masternode staked UTXOs
        for lc in self.utxo_manager.list_locked_collaterals() {
            let amount = if let Ok(utxo) = self.utxo_manager.get_utxo(&lc.outpoint).await {
                utxo.value as f64 / 100_000_000.0
            } else {
                lc.amount as f64 / 100_000_000.0
            };
            let age_seconds = now - lc.locked_at as i64;
            locked.push(json!({
                "txid": hex::encode(lc.outpoint.txid),
                "vout": lc.outpoint.vout,
                "amount": amount,
                "lock_type": "collateral",
                "masternode_address": lc.masternode_address,
                "lock_height": lc.lock_height,
                "locked_at": lc.locked_at,
                "age_seconds": age_seconds,
                "unlock_height": lc.unlock_height
            }));
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

        // Verify the UTXO belongs to this wallet before unlocking
        let local_wallet_address = match self.registry.get_local_masternode().await {
            Some(mn) => Some(mn.masternode.wallet_address),
            None => self.registry.get_local_wallet_address().await,
        };
        if let Ok(utxo) = self.utxo_manager.get_utxo(&outpoint).await {
            if let Some(ref local_addr) = local_wallet_address {
                if utxo.address != *local_addr {
                    return Err(RpcError {
                        code: -8,
                        message: "Cannot unlock a UTXO that does not belong to this wallet"
                            .to_string(),
                    });
                }
            }
        }

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

    /// Release all collateral locks without touching transaction UTXO locks.
    /// Use this when squatters have locked collateral UTXOs and you need to reclaim them.
    /// Safer than forceunlockall which also resets pending/finalized transaction states.
    async fn release_all_collaterals(&self) -> Result<Value, RpcError> {
        let count = self.utxo_manager.unlock_all_collaterals();
        tracing::warn!("🔓 Released {} collateral lock(s) via RPC", count);
        Ok(json!({
            "released": count,
            "message": format!(
                "Released {} collateral lock(s). Masternodes will re-register on next announcement.",
                count
            )
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

    /// Clear stuck finalized transactions from the mempool and revert their UTXO
    /// changes. This is a recovery tool for when nodes have divergent UTXO states
    /// and cannot accept each other's blocks.
    ///
    /// For each stuck finalized TX:
    /// 1. Input UTXOs are restored from SpentFinalized → Unspent
    /// 2. Output UTXOs created by the TX are removed from storage
    /// 3. The TX is removed from both finalized and pending pools
    async fn clear_stuck_transactions(&self) -> Result<Value, RpcError> {
        let finalized_txs = self.consensus.get_finalized_transactions_for_block();

        if finalized_txs.is_empty() {
            return Ok(json!({
                "cleared": 0,
                "inputs_restored": 0,
                "outputs_removed": 0,
                "message": "No finalized transactions in mempool"
            }));
        }

        let mut inputs_restored = 0u64;
        let mut outputs_removed = 0u64;
        let mut cleared_txids = Vec::new();
        let mut total_input_value = 0u64;
        let mut total_output_value = 0u64;
        let mut skipped_txids = Vec::new();

        for tx in &finalized_txs {
            let txid = tx.txid();

            // Pre-flight: verify all input UTXOs exist in storage before touching anything.
            // If any input is missing, the TX can't be safely reversed (coins would be lost).
            let mut tx_input_value = 0u64;
            let mut inputs_ok = true;
            for input in &tx.inputs {
                match self.utxo_manager.get_utxo(&input.previous_output).await {
                    Ok(utxo) => {
                        tx_input_value += utxo.value;
                    }
                    Err(_) => {
                        tracing::warn!(
                            "⚠️ Skipping TX {}: input UTXO {} missing from storage (unsafe to clear)",
                            hex::encode(txid),
                            input.previous_output
                        );
                        inputs_ok = false;
                        break;
                    }
                }
            }

            if !inputs_ok {
                skipped_txids.push(hex::encode(txid));
                continue;
            }

            let tx_output_value: u64 = tx.outputs.iter().map(|o| o.value).sum();
            total_input_value += tx_input_value;
            total_output_value += tx_output_value;

            // Restore input UTXOs: SpentFinalized → Unspent
            for input in &tx.inputs {
                let outpoint = &input.previous_output;
                if matches!(
                    self.utxo_manager.get_state(outpoint),
                    Some(
                        crate::types::UTXOState::SpentFinalized { .. }
                            | crate::types::UTXOState::SpentPending { .. }
                            | crate::types::UTXOState::Locked { .. }
                    )
                ) {
                    if self.utxo_manager.is_collateral_locked(outpoint) {
                        tracing::warn!(
                            "⚠️ Skipping collateral UTXO {} during stuck TX cleanup",
                            outpoint
                        );
                        continue;
                    }
                    self.utxo_manager
                        .update_state(outpoint, crate::types::UTXOState::Unspent);
                    inputs_restored += 1;
                }
            }

            // Remove output UTXOs that were created when this TX was auto-finalized
            for (idx, _output) in tx.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid,
                    vout: idx as u32,
                };
                if self.utxo_manager.get_state(&outpoint).is_some() {
                    if let Err(e) = self.utxo_manager.remove_utxo(&outpoint).await {
                        tracing::warn!(
                            "⚠️ Failed to remove output UTXO {} from stuck TX: {}",
                            outpoint,
                            e
                        );
                    } else {
                        outputs_removed += 1;
                    }
                }
            }

            cleared_txids.push(hex::encode(txid));
        }

        // Clear from finalized and pending pools
        let txids: Vec<crate::types::Hash256> = finalized_txs
            .iter()
            .filter(|tx| {
                let txid_hex = hex::encode(tx.txid());
                cleared_txids.contains(&txid_hex)
            })
            .map(|tx| tx.txid())
            .collect();
        self.consensus.clear_finalized_txs(&txids);

        let fee_value = total_input_value.saturating_sub(total_output_value);

        tracing::warn!(
            "🧹 Cleared {} stuck finalized transactions: restored {} input UTXOs, removed {} output UTXOs \
             (input_value={}, output_value={}, fees={})",
            cleared_txids.len(),
            inputs_restored,
            outputs_removed,
            total_input_value,
            total_output_value,
            fee_value
        );

        if !skipped_txids.is_empty() {
            tracing::warn!(
                "⚠️ Skipped {} TX(s) with missing input UTXOs (unsafe to clear): {:?}",
                skipped_txids.len(),
                skipped_txids
            );
        }

        Ok(json!({
            "cleared": cleared_txids.len(),
            "skipped": skipped_txids.len(),
            "inputs_restored": inputs_restored,
            "outputs_removed": outputs_removed,
            "total_input_value": total_input_value,
            "total_output_value": total_output_value,
            "fees_returned": fee_value,
            "transactions": cleared_txids,
            "skipped_transactions": skipped_txids,
            "message": format!(
                "Cleared {} stuck transactions (skipped {}), restored {} inputs (value: {}), removed {} outputs (value: {})",
                cleared_txids.len(), skipped_txids.len(), inputs_restored, total_input_value, outputs_removed, total_output_value
            )
        }))
    }

    /// Drop a specific pending transaction from the mempool and release its UTXO locks.
    /// Only works on transactions that are still pending (not yet finalized).
    /// For finalized transactions use clearstucktransactions instead.
    /// Params: [txid]
    async fn drop_transaction(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Usage: droptransaction <txid>".to_string(),
            })?;

        let txid_bytes = hex::decode(txid_str).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid txid: not valid hex".to_string(),
        })?;
        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "Invalid txid: must be exactly 64 hex characters".to_string(),
            });
        }
        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);

        let tx = match self.consensus.tx_pool.get_transaction(&txid) {
            Some(tx) => tx,
            None => {
                return Err(RpcError {
                    code: -5,
                    message: format!("Transaction {} not found in mempool", txid_str),
                });
            }
        };

        if self.consensus.tx_pool.is_finalized(&txid) {
            return Err(RpcError {
                code: -1,
                message:
                    "Transaction is already finalized — use clearstucktransactions to roll it back"
                        .to_string(),
            });
        }

        // Unlock all input UTXOs that were locked by this pending tx
        let mut unlocked = 0usize;
        for input in &tx.inputs {
            if self
                .utxo_manager
                .unlock_utxo(&input.previous_output, &txid)
                .is_ok()
            {
                unlocked += 1;
            }
        }

        // Remove from the pending pool and mark as rejected so it isn't re-accepted
        self.consensus
            .tx_pool
            .reject_transaction(txid, "Manually dropped via droptransaction RPC".to_string());

        tracing::warn!(
            "🗑️ Dropped pending transaction {} — released {} UTXO lock(s)",
            txid_str,
            unlocked
        );

        Ok(json!({
            "txid": txid_str,
            "dropped": true,
            "inputs_unlocked": unlocked,
            "message": format!(
                "Dropped pending transaction {} and released {} UTXO lock(s). \
                 You can now create a new transaction spending the same UTXOs.",
                txid_str, unlocked
            )
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

    /// Create a payment request URI that can be shared with the payer.
    /// The URI includes the recipient's address, public key, amount, and optional memo.
    async fn create_payment_request(&self, params: &[Value]) -> Result<Value, RpcError> {
        // createpaymentrequest amount [memo] [label]
        let amount = params
            .first()
            .and_then(|v| v.as_f64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected amount (in TIME)".to_string(),
            })?;

        if amount <= 0.0 {
            return Err(RpcError {
                code: -32602,
                message: "Amount must be positive".to_string(),
            });
        }

        let memo = params.get(1).and_then(|v| v.as_str()).unwrap_or("");
        let label = params.get(2).and_then(|v| v.as_str()).unwrap_or("");

        // Get our wallet address
        let wallet_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address)
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode - no wallet address".to_string(),
            })?;

        // Get our Ed25519 public key
        let signing_key = self
            .consensus
            .get_wallet_signing_key()
            .ok_or_else(|| RpcError {
                code: -4,
                message: "No signing key available".to_string(),
            })?;
        let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());

        // Build URI: timecoin:ADDRESS?amount=X&pubkey=HEX[&memo=TEXT][&label=TEXT]
        let mut uri = format!(
            "timecoin:{}?amount={}&pubkey={}",
            wallet_address, amount, pubkey_hex
        );
        if !memo.is_empty() {
            uri.push_str(&format!("&memo={}", urlencoding::encode(memo)));
        }
        if !label.is_empty() {
            uri.push_str(&format!("&label={}", urlencoding::encode(label)));
        }

        Ok(json!({
            "uri": uri,
            "address": wallet_address,
            "pubkey": pubkey_hex,
            "amount": amount,
            "memo": memo,
            "label": label,
        }))
    }

    /// Pay a payment request URI. Parses the URI, caches the recipient's pubkey,
    /// and sends funds with an encrypted memo.
    async fn pay_payment_request(&self, params: &[Value]) -> Result<Value, RpcError> {
        // paypaymentrequest "timecoin:ADDRESS?amount=X&pubkey=HEX&memo=TEXT" [memo_override]
        let uri = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Invalid params: expected payment request URI".to_string(),
            })?;

        // Parse the URI
        let stripped = uri.strip_prefix("timecoin:").ok_or_else(|| RpcError {
            code: -32602,
            message: "Invalid URI: must start with 'timecoin:'".to_string(),
        })?;

        // Split address from query params
        let (address, query) = stripped.split_once('?').ok_or_else(|| RpcError {
            code: -32602,
            message: "Invalid URI: missing parameters (expected ?amount=&pubkey=)".to_string(),
        })?;

        // Parse query parameters
        let mut amount: Option<f64> = None;
        let mut pubkey_hex: Option<String> = None;
        let mut memo: Option<String> = None;
        let mut label: Option<String> = None;

        for param in query.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                match key {
                    "amount" => {
                        amount = value.parse().ok();
                    }
                    "pubkey" => {
                        pubkey_hex = Some(value.to_string());
                    }
                    "memo" => {
                        let decoded = urlencoding::decode(value).unwrap_or(value.into());
                        memo = Some(decoded.into_owned());
                    }
                    "label" => {
                        let decoded = urlencoding::decode(value).unwrap_or(value.into());
                        label = Some(decoded.into_owned());
                    }
                    _ => {} // ignore unknown params for forward compatibility
                }
            }
        }

        let amount = amount.ok_or_else(|| RpcError {
            code: -32602,
            message: "Invalid URI: missing or invalid 'amount' parameter".to_string(),
        })?;

        // Cache the recipient's pubkey if provided (enables memo encryption)
        if let Some(ref pk_hex) = pubkey_hex {
            if let Ok(pk_bytes) = hex::decode(pk_hex) {
                if pk_bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&pk_bytes);
                    self.consensus.utxo_manager.register_pubkey(address, arr);
                    tracing::info!(
                        address = address,
                        "Cached recipient pubkey from payment request"
                    );
                }
            }
        }

        // Allow the payer to override the memo
        let memo_override = params.get(1).and_then(|v| v.as_str());
        let final_memo = memo_override.map(|s| s.to_string()).or(memo);

        // Display what we're paying
        let label_display = label.as_deref().unwrap_or("");
        tracing::info!(
            address = address,
            amount = amount,
            memo = final_memo.as_deref().unwrap_or(""),
            label = label_display,
            "Paying payment request"
        );

        // Get wallet address
        let wallet_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address)
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode - no wallet address".to_string(),
            })?;

        // Send the coins with the memo
        let result = self
            .send_coins(
                &wallet_address,
                address,
                amount,
                false,
                false,
                final_memo.as_deref(),
            )
            .await?;

        // Augment the response with payment request info
        let mut response = result.clone();
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "payment_request".to_string(),
                json!({
                    "address": address,
                    "amount": amount,
                    "memo": final_memo,
                    "label": label,
                    "pubkey_cached": pubkey_hex.is_some(),
                }),
            );
        }

        Ok(response)
    }

    /// Accept a payment request from a wallet, store it, and broadcast to peers.
    ///
    /// Params: [object] where object contains:
    ///   requester_address  (required) — address of the party requesting payment
    ///   payer_address      (required) — address of the party being asked to pay
    ///   amount             (required) — amount in satoshis (u64)
    ///   id                 (optional) — client-generated UUID; computed from hash if absent
    ///   memo               (optional) — human-readable description
    ///   requester_name     (optional) — display name of the requester
    ///   pubkey_hex         (optional) — Ed25519 public key hex; enables signature verification
    ///   signature_hex      (optional) — Ed25519 signature hex over canonical fields
    ///   timestamp          (optional) — Unix timestamp; defaults to now
    async fn send_payment_request(&self, params: &[Value]) -> Result<Value, RpcError> {
        let obj = params
            .first()
            .and_then(|v| v.as_object())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected JSON object as first parameter".to_string(),
            })?;

        let from_address = obj
            .get("requester_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing requester_address".to_string(),
            })?;
        let to_address = obj
            .get("payer_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing payer_address".to_string(),
            })?;
        let amount = obj
            .get("amount")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing or invalid amount (expected u64 satoshis)".to_string(),
            })?;
        let memo = obj.get("memo").and_then(|v| v.as_str()).unwrap_or("");
        let requester_name = obj
            .get("requester_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pubkey_hex_opt = obj.get("pubkey_hex").and_then(|v| v.as_str()).unwrap_or("");
        let signature_hex_opt = obj
            .get("signature_hex")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = obj
            .get("timestamp")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| chrono::Utc::now().timestamp());

        // Compute deterministic ID from hash, or use client-supplied id
        use sha2::{Digest, Sha256};
        let id = if let Some(client_id) = obj.get("id").and_then(|v| v.as_str()) {
            client_id.to_string()
        } else {
            let mut hasher = Sha256::new();
            hasher.update(from_address.as_bytes());
            hasher.update(to_address.as_bytes());
            hasher.update(amount.to_le_bytes());
            hasher.update(timestamp.to_le_bytes());
            hex::encode(hasher.finalize())
        };

        // If pubkey + signature are provided, verify them (optional but preferred)
        let mut verified_pubkey: Option<[u8; 32]> = None;
        if !pubkey_hex_opt.is_empty() && !signature_hex_opt.is_empty() {
            let pubkey_bytes = hex::decode(pubkey_hex_opt).unwrap_or_default();
            let sig_bytes = hex::decode(signature_hex_opt).unwrap_or_default();
            if pubkey_bytes.len() == 32 && sig_bytes.len() == 64 {
                let mut pubkey = [0u8; 32];
                pubkey.copy_from_slice(&pubkey_bytes);
                let mut signature = [0u8; 64];
                signature.copy_from_slice(&sig_bytes);
                if let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&pubkey) {
                    let ed_signature = ed25519_dalek::Signature::from_bytes(&signature);
                    let mut sign_data = Vec::new();
                    sign_data.extend_from_slice(id.as_bytes());
                    sign_data.extend_from_slice(from_address.as_bytes());
                    sign_data.extend_from_slice(to_address.as_bytes());
                    sign_data.extend_from_slice(&amount.to_le_bytes());
                    sign_data.extend_from_slice(memo.as_bytes());
                    sign_data.extend_from_slice(&timestamp.to_le_bytes());
                    if verifying_key
                        .verify_strict(&sign_data, &ed_signature)
                        .is_err()
                    {
                        return Err(RpcError {
                            code: -1,
                            message: "Invalid signature — request may be spoofed".to_string(),
                        });
                    }
                    verified_pubkey = Some(pubkey);
                }
            }
        }

        let expires = timestamp + 86400; // 24 hours

        let request = crate::network::message::PaymentRequest {
            id: id.clone(),
            from_address: from_address.to_string(),
            to_address: to_address.to_string(),
            amount,
            memo: memo.to_string(),
            requester_name,
            pubkey_hex: pubkey_hex_opt.to_string(),
            signature_hex: signature_hex_opt.to_string(),
            timestamp,
            expires,
        };

        // Cache the requester's pubkey for future memo encryption (if provided)
        if let Some(pubkey) = verified_pubkey {
            self.consensus
                .utxo_manager
                .register_pubkey(from_address, pubkey);
        }

        // Store locally
        let stored = self.consensus.store_payment_request(request.clone());
        if !stored {
            return Err(RpcError {
                code: -1,
                message: "Request already exists, expired, or address limit reached".to_string(),
            });
        }

        // Broadcast to peers
        self.consensus
            .broadcast_payment_request(request.clone())
            .await;

        // Push WS notification to payer if subscribed
        if let Some(ref tx_sender) = self.tx_event_sender {
            let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                txid: format!("pr:{}", id),
                outputs: vec![crate::rpc::websocket::TxOutputInfo {
                    address: to_address.to_string(),
                    amount: amount as f64 / 100_000_000.0,
                    index: 0,
                }],
                timestamp,
                status: crate::rpc::websocket::TxEventStatus::PaymentRequest {
                    from_address: from_address.to_string(),
                    memo: memo.to_string(),
                    requester_name: request.requester_name.clone(),
                    pubkey_hex: pubkey_hex_opt.to_string(),
                    expires,
                },
            });
        }

        Ok(json!({
            "id": id,
            "status": "sent",
            "expires": expires,
        }))
    }

    /// Return pending payment requests for a set of addresses.
    /// Params: [addresses[]]
    async fn get_payment_requests(&self, params: &[Value]) -> Result<Value, RpcError> {
        let addresses: Vec<String> = params
            .first()
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected array of addresses".to_string(),
            })?;

        let requests = self.consensus.get_payment_requests_for(&addresses);

        let results: Vec<Value> = requests
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "from_address": r.from_address,
                    "to_address": r.to_address,
                    "amount": r.amount,
                    "memo": r.memo,
                    "requester_name": r.requester_name,
                    "pubkey": r.pubkey_hex,
                    "timestamp": r.timestamp,
                    "expires": r.expires,
                })
            })
            .collect();

        Ok(json!(results))
    }

    /// Acknowledge (remove) a payment request by id.
    /// Params: [request_id, status]  (status = "paid" or "declined")
    async fn acknowledge_payment_request(&self, params: &[Value]) -> Result<Value, RpcError> {
        let request_id = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing request_id".to_string(),
            })?;
        let status = params
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("acknowledged");

        let removed = self.consensus.remove_payment_request(request_id);

        Ok(json!({
            "id": request_id,
            "status": status,
            "removed": removed,
        }))
    }

    /// Payer responds to a pending payment request (accept or decline).
    ///
    /// Params: [object] where object contains:
    ///   id           (required) — payment request id
    ///   payer_address (required) — address of the payer responding
    ///   accepted     (required) — true if accepted, false if declined
    ///   txid         (optional) — transaction id if accepted and paid
    async fn respond_payment_request(&self, params: &[Value]) -> Result<Value, RpcError> {
        let obj = params
            .first()
            .and_then(|v| v.as_object())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected JSON object as first parameter".to_string(),
            })?;

        let request_id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing id".to_string(),
            })?;
        let payer_address = obj
            .get("payer_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing payer_address".to_string(),
            })?;
        let accepted = obj
            .get("accepted")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing accepted (bool)".to_string(),
            })?;
        let txid: Option<String> = obj
            .get("txid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Look up requester_address before removing (needed for peer broadcast + WS routing)
        let requester_address = self
            .consensus
            .get_payment_request_requester(request_id)
            .unwrap_or_default();

        // Remove from storage (request is resolved)
        self.consensus.remove_payment_request(request_id);

        // Relay to peers so the requester's node gets notified
        self.consensus
            .broadcast_payment_request_response(
                request_id.to_string(),
                requester_address.to_string(),
                payer_address.to_string(),
                accepted,
                txid.clone(),
            )
            .await;

        // Push WS notification to the requester if they're subscribed on this node
        // (route to requester_address via the outputs field)
        if let Some(ref tx_sender) = self.tx_event_sender {
            let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                txid: format!("pr-resp:{}", request_id),
                outputs: vec![crate::rpc::websocket::TxOutputInfo {
                    address: requester_address.to_string(),
                    amount: 0.0,
                    index: 0,
                }],
                timestamp: chrono::Utc::now().timestamp(),
                status: crate::rpc::websocket::TxEventStatus::PaymentRequestResponse {
                    request_id: request_id.to_string(),
                    payer_address: payer_address.to_string(),
                    accepted,
                    txid,
                },
            });
        }

        Ok(json!({
            "id": request_id,
            "accepted": accepted,
            "status": if accepted { "accepted" } else { "declined" },
        }))
    }

    /// Requester cancels their own pending payment request.
    ///
    /// Params: [object] where object contains:
    ///   id                (required) — payment request id
    ///   requester_address (required) — address of the requester (must match stored request)
    async fn cancel_payment_request(&self, params: &[Value]) -> Result<Value, RpcError> {
        let obj = params
            .first()
            .and_then(|v| v.as_object())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected JSON object as first parameter".to_string(),
            })?;

        let request_id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing id".to_string(),
            })?;
        let requester_address = obj
            .get("requester_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing requester_address".to_string(),
            })?;

        // Look up the payer address before removing (needed for WS notification)
        let payer_address = self
            .consensus
            .get_payment_request_payer(request_id)
            .unwrap_or_default();

        let removed = self.consensus.remove_payment_request(request_id);

        // Relay cancellation to peers
        self.consensus
            .broadcast_payment_request_cancelled(
                request_id.to_string(),
                requester_address.to_string(),
            )
            .await;

        // Push WS notification to the payer if subscribed on this node
        if !payer_address.is_empty() {
            if let Some(ref tx_sender) = self.tx_event_sender {
                let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                    txid: format!("pr-cancel:{}", request_id),
                    outputs: vec![crate::rpc::websocket::TxOutputInfo {
                        address: payer_address.clone(),
                        amount: 0.0,
                        index: 0,
                    }],
                    timestamp: chrono::Utc::now().timestamp(),
                    status: crate::rpc::websocket::TxEventStatus::PaymentRequestCancelled {
                        request_id: request_id.to_string(),
                        requester_address: requester_address.to_string(),
                    },
                });
            }
        }

        Ok(json!({
            "id": request_id,
            "status": "cancelled",
            "removed": removed,
        }))
    }

    /// Mark a payment request as viewed by the payer (notifies the requester).
    ///
    /// Params: [object] where object contains:
    ///   id           (required) — payment request id
    ///   payer_address (required) — address of the payer who viewed the request
    async fn mark_payment_request_viewed(&self, params: &[Value]) -> Result<Value, RpcError> {
        let obj = params
            .first()
            .and_then(|v| v.as_object())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected JSON object as first parameter".to_string(),
            })?;

        let request_id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing id".to_string(),
            })?;
        let payer_address = obj
            .get("payer_address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing payer_address".to_string(),
            })?;

        // Look up requester_address from stored request for peer broadcast + WS routing
        let requester_address = self
            .consensus
            .get_payment_request_requester(request_id)
            .unwrap_or_default();

        // Relay to peers so the requester's node gets notified
        self.consensus
            .broadcast_payment_request_viewed(
                request_id.to_string(),
                requester_address.to_string(),
                payer_address.to_string(),
            )
            .await;

        // Push WS notification to the requester if subscribed on this node
        if let Some(ref tx_sender) = self.tx_event_sender {
            let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                txid: format!("pr-view:{}", request_id),
                outputs: vec![crate::rpc::websocket::TxOutputInfo {
                    address: requester_address.to_string(),
                    amount: 0.0,
                    index: 0,
                }],
                timestamp: chrono::Utc::now().timestamp(),
                status: crate::rpc::websocket::TxEventStatus::PaymentRequestViewed {
                    request_id: request_id.to_string(),
                    payer_address: payer_address.to_string(),
                },
            });
        }

        Ok(json!({ "id": request_id, "status": "viewed" }))
    }

    // ── Governance RPCs ───────────────────────────────────────────────────────

    async fn submit_proposal(&self, params: &[Value]) -> Result<Value, RpcError> {
        use crate::governance::{GovernanceProposal, ProposalPayload, VOTING_PERIOD_BLOCKS};

        let obj = params
            .first()
            .and_then(Value::as_object)
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected object: {type, ...}".to_string(),
            })?;

        let prop_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = match prop_type {
            "treasury_spend" => {
                let recipient = obj
                    .get("recipient")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "Missing recipient".into(),
                    })?
                    .to_string();
                let amount = obj
                    .get("amount")
                    .and_then(Value::as_f64)
                    .map(|a| (a * 100_000_000.0) as u64)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "Missing amount".into(),
                    })?;
                let description = obj
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                ProposalPayload::TreasurySpend {
                    recipient,
                    amount,
                    description,
                }
            }
            "fee_schedule_change" => {
                let new_min_fee = obj
                    .get("new_min_fee")
                    .and_then(Value::as_f64)
                    .map(|f| (f * 100_000_000.0) as u64)
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "Missing new_min_fee".into(),
                    })?;
                let raw_tiers =
                    obj.get("new_tiers")
                        .and_then(Value::as_array)
                        .ok_or_else(|| RpcError {
                            code: -32602,
                            message: "Missing new_tiers".into(),
                        })?;
                let mut new_tiers: Vec<(u64, u64)> = Vec::new();
                for t in raw_tiers {
                    let arr = t.as_array().ok_or_else(|| RpcError {
                        code: -32602,
                        message: "Each tier must be [upper_bound_TIME, rate_bps]".into(),
                    })?;
                    if arr.len() != 2 {
                        return Err(RpcError {
                            code: -32602,
                            message: "Tier must have 2 elements".into(),
                        });
                    }
                    let upper = (arr[0].as_f64().unwrap_or(0.0) * 100_000_000.0) as u64;
                    let bps = arr[1].as_u64().unwrap_or(0);
                    new_tiers.push((upper, bps));
                }
                ProposalPayload::FeeScheduleChange {
                    new_min_fee,
                    new_tiers,
                }
            }
            "emission_rate_change" => {
                // Accept new_time_per_block (decimal TIME) or new_satoshis_per_block (integer)
                let new_satoshis_per_block = if let Some(time_val) =
                    obj.get("new_time_per_block").and_then(Value::as_f64)
                {
                    (time_val * 100_000_000.0) as u64
                } else {
                    obj.get("new_satoshis_per_block")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| RpcError {
                            code: -32602,
                            message: "Missing new_time_per_block or new_satoshis_per_block".into(),
                        })?
                };
                let description = obj
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                ProposalPayload::EmissionRateChange {
                    new_satoshis_per_block,
                    description,
                }
            }
            other => {
                return Err(RpcError {
                    code: -32602,
                    message: format!("Unknown proposal type: {other}. Valid types: treasury_spend, fee_schedule_change, emission_rate_change"),
                })
            }
        };

        let signing_key = self
            .consensus
            .get_wallet_signing_key()
            .ok_or_else(|| RpcError {
                code: -32001,
                message: "No signing key available — wallet not unlocked".to_string(),
            })?;
        let pubkey = signing_key.verifying_key().to_bytes();

        let payload_bytes = bincode::serialize(&payload).map_err(|e| RpcError {
            code: -32603,
            message: format!("Serialization error: {e}"),
        })?;

        let height = self.blockchain.get_height();
        let id = GovernanceProposal::compute_id(&payload_bytes, &pubkey, height);

        let mut proposal = GovernanceProposal {
            id,
            payload,
            submitter_address: self.registry.get_local_address().await.unwrap_or_default(),
            submitter_pubkey: pubkey,
            submitter_signature: [0u8; 64],
            submit_height: height,
            vote_end_height: height + VOTING_PERIOD_BLOCKS,
            status: crate::governance::ProposalStatus::Active,
        };
        proposal.sign(&signing_key);

        let gov = self.blockchain.governance().ok_or_else(|| RpcError {
            code: -32603,
            message: "Governance subsystem not initialized".to_string(),
        })?;

        let treasury = self.blockchain.get_treasury_balance();
        gov.submit_proposal(proposal.clone(), &self.registry, treasury)
            .await
            .map_err(|e| RpcError {
                code: -32603,
                message: e,
            })?;

        // Broadcast to peers
        if let Some(registry) = self.blockchain.get_peer_registry().await {
            let _ = registry
                .broadcast(crate::network::message::NetworkMessage::GovernanceProposal(
                    proposal.clone(),
                ))
                .await;
        }

        Ok(json!({
            "proposal_id": hex::encode(proposal.id),
            "vote_end_height": proposal.vote_end_height,
            "status": "active",
        }))
    }

    async fn vote_proposal(&self, params: &[Value]) -> Result<Value, RpcError> {
        use crate::governance::GovernanceVote;

        let id_hex = params
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected [proposal_id_hex, approve_bool]".to_string(),
            })?;
        let approve = params
            .get(1)
            .and_then(Value::as_bool)
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected approve parameter (true/false)".to_string(),
            })?;

        let id_bytes = hex::decode(id_hex).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid proposal_id hex".to_string(),
        })?;
        if id_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "proposal_id must be 32 bytes".into(),
            });
        }
        let mut proposal_id = [0u8; 32];
        proposal_id.copy_from_slice(&id_bytes);

        let signing_key = self
            .consensus
            .get_wallet_signing_key()
            .ok_or_else(|| RpcError {
                code: -32001,
                message: "No signing key available — wallet not unlocked".to_string(),
            })?;
        let pubkey = signing_key.verifying_key().to_bytes();
        let height = self.blockchain.get_height();
        let voter_address = self.registry.get_local_address().await.unwrap_or_default();

        let mut vote = GovernanceVote {
            proposal_id,
            voter_address,
            voter_pubkey: pubkey,
            approve,
            vote_height: height,
            signature: [0u8; 64],
        };
        vote.sign(&signing_key);

        let gov = self.blockchain.governance().ok_or_else(|| RpcError {
            code: -32603,
            message: "Governance subsystem not initialized".to_string(),
        })?;

        gov.record_vote(vote.clone(), &self.registry)
            .await
            .map_err(|e| RpcError {
                code: -32603,
                message: e,
            })?;

        if let Some(registry) = self.blockchain.get_peer_registry().await {
            let _ = registry
                .broadcast(crate::network::message::NetworkMessage::GovernanceVote(
                    vote,
                ))
                .await;
        }

        Ok(json!({
            "proposal_id": id_hex,
            "approve": approve,
            "status": "recorded",
        }))
    }

    async fn list_proposals(&self, params: &[Value]) -> Result<Value, RpcError> {
        let filter = params.first().and_then(Value::as_str);

        let gov = self.blockchain.governance().ok_or_else(|| RpcError {
            code: -32603,
            message: "Governance subsystem not initialized".to_string(),
        })?;

        let proposals = gov.list_proposals().await;
        let total_weight = crate::governance::GovernanceState::total_weight(&self.registry).await;

        let filtered: Vec<Value> = proposals
            .iter()
            .filter(|p| match filter {
                Some("active") => p.status == crate::governance::ProposalStatus::Active,
                Some("failed") => p.status == crate::governance::ProposalStatus::Failed,
                Some("executed") => p.status == crate::governance::ProposalStatus::Executed,
                Some("passed") => {
                    matches!(p.status, crate::governance::ProposalStatus::Passed { .. })
                }
                _ => true,
            })
            .map(|p| {
                let type_str = match &p.payload {
                    crate::governance::ProposalPayload::TreasurySpend { .. } => "treasury_spend",
                    crate::governance::ProposalPayload::FeeScheduleChange { .. } => {
                        "fee_schedule_change"
                    }
                    crate::governance::ProposalPayload::EmissionRateChange { .. } => {
                        "emission_rate_change"
                    }
                };
                let status_str = match &p.status {
                    crate::governance::ProposalStatus::Active => "active".to_string(),
                    crate::governance::ProposalStatus::Passed { execute_at_height } => {
                        format!("passed (executes at {})", execute_at_height)
                    }
                    crate::governance::ProposalStatus::Failed => "failed".to_string(),
                    crate::governance::ProposalStatus::Executed => "executed".to_string(),
                };
                json!({
                    "id": hex::encode(p.id),
                    "type": type_str,
                    "submitter": p.submitter_address,
                    "submit_height": p.submit_height,
                    "vote_end_height": p.vote_end_height,
                    "status": status_str,
                    "total_weight": total_weight,
                })
            })
            .collect();

        Ok(Value::Array(filtered))
    }

    async fn get_proposal(&self, params: &[Value]) -> Result<Value, RpcError> {
        let id_hex = params
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected proposal_id_hex".to_string(),
            })?;

        let id_bytes = hex::decode(id_hex).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid hex".to_string(),
        })?;
        if id_bytes.len() != 32 {
            return Err(RpcError {
                code: -32602,
                message: "proposal_id must be 32 bytes".into(),
            });
        }
        let mut id = [0u8; 32];
        id.copy_from_slice(&id_bytes);

        let gov = self.blockchain.governance().ok_or_else(|| RpcError {
            code: -32603,
            message: "Governance subsystem not initialized".to_string(),
        })?;

        let proposal = gov.get_proposal(&id).await.ok_or_else(|| RpcError {
            code: -32602,
            message: format!("Proposal {id_hex} not found"),
        })?;

        let votes = gov.get_votes_for(&id).await;
        let yes_weight = gov.yes_weight(&id, &self.registry).await;
        let total_weight = crate::governance::GovernanceState::total_weight(&self.registry).await;

        let payload_json = match &proposal.payload {
            crate::governance::ProposalPayload::TreasurySpend {
                recipient,
                amount,
                description,
            } => json!({
                "type": "treasury_spend",
                "recipient": recipient,
                "amount": *amount as f64 / 100_000_000.0,
                "amount_satoshis": amount,
                "description": description,
            }),
            crate::governance::ProposalPayload::FeeScheduleChange {
                new_min_fee,
                new_tiers,
            } => json!({
                "type": "fee_schedule_change",
                "new_min_fee": *new_min_fee as f64 / 100_000_000.0,
                "new_min_fee_satoshis": new_min_fee,
                "new_tiers": new_tiers,
            }),
            crate::governance::ProposalPayload::EmissionRateChange {
                new_satoshis_per_block,
                description,
            } => json!({
                "type": "emission_rate_change",
                "new_time_per_block": *new_satoshis_per_block as f64 / 100_000_000.0,
                "new_satoshis_per_block": new_satoshis_per_block,
                "description": description,
            }),
        };

        let status_str = match &proposal.status {
            crate::governance::ProposalStatus::Active => "active".to_string(),
            crate::governance::ProposalStatus::Passed { execute_at_height } => {
                format!("passed (executes at {})", execute_at_height)
            }
            crate::governance::ProposalStatus::Failed => "failed".to_string(),
            crate::governance::ProposalStatus::Executed => "executed".to_string(),
        };

        let votes_json: Vec<Value> = votes
            .iter()
            .map(|v| {
                json!({
                    "voter": v.voter_address,
                    "approve": v.approve,
                    "vote_height": v.vote_height,
                })
            })
            .collect();

        Ok(json!({
            "id": id_hex,
            "payload": payload_json,
            "submitter": proposal.submitter_address,
            "submit_height": proposal.submit_height,
            "vote_end_height": proposal.vote_end_height,
            "status": status_str,
            "yes_weight": yes_weight,
            "total_weight": total_weight,
            "quorum_pct": (yes_weight * 100).checked_div(total_weight).unwrap_or(0),
            "votes": votes_json,
        }))
    }

    // -------------------------------------------------------------------------
    // Bitcoin-compatible additions
    // -------------------------------------------------------------------------

    /// Scan the chain tip-to-genesis for a block whose hash matches `target_hash`.
    /// O(n) — there is no hash→height index yet. Acceptable for current chain lengths;
    /// a sled reverse-index should be added when the chain grows beyond ~100k blocks.
    async fn find_block_by_hash(
        &self,
        target_hash: [u8; 32],
    ) -> Option<crate::block::types::Block> {
        let current_height = self.blockchain.get_height();
        for h in (0..=current_height).rev() {
            if let Ok(block) = self.blockchain.get_block_by_height(h).await {
                if block.hash() == target_hash {
                    return Some(block);
                }
            }
        }
        None
    }

    /// `getblockheader "hash"|height`
    ///
    /// Returns the block header without the full transaction list.
    /// Accepts either a 64-char hex block hash or a numeric height, matching
    /// the same dual-dispatch logic as `getblock`.
    async fn get_block_header(&self, params: &[Value]) -> Result<Value, RpcError> {
        let first = params.first().ok_or_else(|| RpcError {
            code: -32602,
            message: "Expected block hash (string) or height (number)".to_string(),
        })?;

        let block = if let Some(hash_str) = first.as_str() {
            let hash_bytes = hex::decode(hash_str).map_err(|_| RpcError {
                code: -8,
                message: "Invalid block hash encoding".to_string(),
            })?;
            if hash_bytes.len() != 32 {
                return Err(RpcError {
                    code: -8,
                    message: "Block hash must be 32 bytes (64 hex chars)".to_string(),
                });
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&hash_bytes);
            self.find_block_by_hash(hash)
                .await
                .ok_or_else(|| RpcError {
                    code: -5,
                    message: "Block not found".to_string(),
                })?
        } else if let Some(height) = first.as_u64() {
            self.blockchain
                .get_block_by_height(height)
                .await
                .map_err(|e| RpcError {
                    code: -5,
                    message: format!("Block not found: {}", e),
                })?
        } else {
            return Err(RpcError {
                code: -32602,
                message: "Expected block hash (string) or height (number)".to_string(),
            });
        };

        let height = block.header.height;
        let current_height = self.blockchain.get_height();
        let block_hash = block.hash();
        let next_hash = if height < current_height {
            self.blockchain
                .get_block_by_height(height + 1)
                .await
                .ok()
                .map(|b| hex::encode(b.hash()))
        } else {
            None
        };

        Ok(json!({
            "hash": hex::encode(block_hash),
            "height": height,
            "version": block.header.version,
            "previousblockhash": hex::encode(block.header.previous_hash),
            "nextblockhash": next_hash,
            "merkleroot": hex::encode(block.header.merkle_root),
            "time": block.header.timestamp,
            "confirmations": (current_height as i64 - height as i64 + 1).max(0),
            "nTx": block.transactions.len(),
            "difficulty": 1.0,
            "chainwork": format!("{:064x}", height),
        }))
    }

    /// `getpeerblock "peer_ip" height`
    ///
    /// Fetches a block directly from a connected peer over P2P and returns it.
    /// Useful for inspecting what genesis (or any block) a peer claims to have
    /// without needing SSH access to that machine.
    ///
    /// # Arguments
    /// * `peer_ip` — IP address of a currently-connected peer (e.g. "47.82.254.82")
    /// * `height`  — Block height to fetch (0 = genesis)
    async fn get_peer_block(&self, params: &[Value]) -> Result<Value, RpcError> {
        let peer_ip = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected peer IP address".to_string(),
            })?;
        let height = params
            .get(1)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected block height (number)".to_string(),
            })?;

        let registry = self
            .blockchain
            .get_peer_registry()
            .await
            .ok_or_else(|| RpcError {
                code: -1,
                message: "Peer registry not available".to_string(),
            })?;

        let msg = crate::network::message::NetworkMessage::GetBlocks(height, height);
        let response = registry
            .send_and_await_response(peer_ip, msg, 15)
            .await
            .map_err(|e| RpcError {
                code: -1,
                message: format!("Failed to fetch block from peer {peer_ip}: {e}"),
            })?;

        let block = match response {
            crate::network::message::NetworkMessage::BlocksResponse(mut blocks) => {
                blocks.pop().ok_or_else(|| RpcError {
                    code: -5,
                    message: format!("Peer {peer_ip} has no block at height {height}"),
                })?
            }
            crate::network::message::NetworkMessage::BlockResponse(block) => block,
            other => {
                return Err(RpcError {
                    code: -1,
                    message: format!(
                        "Unexpected response from peer {peer_ip}: {}",
                        other.message_type()
                    ),
                })
            }
        };

        let block_hash = block.hash();
        Ok(json!({
            "peer": peer_ip,
            "height": block.header.height,
            "hash": hex::encode(block_hash),
            "previousblockhash": hex::encode(block.header.previous_hash),
            "time": block.header.timestamp,
            "merkleroot": hex::encode(block.header.merkle_root),
            "nTx": block.transactions.len(),
            "block_reward": block.header.block_reward,
            "masternode_rewards": block.masternode_rewards.iter().map(|(addr, amount)| {
                json!({ "address": addr, "amount": amount })
            }).collect::<Vec<_>>(),
        }))
    }

    /// `gettxout "txid" vout [include_mempool]`
    ///
    /// Returns details about an unspent transaction output.
    /// Returns `null` if the output is spent or does not exist.
    async fn get_txout(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_str = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected txid".to_string(),
            })?;
        let vout = params
            .get(1)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected vout".to_string(),
            })? as u32;
        let include_mempool = params.get(2).and_then(|v| v.as_bool()).unwrap_or(true);

        let txid_bytes = hex::decode(txid_str).map_err(|_| RpcError {
            code: -8,
            message: "Invalid txid encoding".to_string(),
        })?;
        if txid_bytes.len() != 32 {
            return Err(RpcError {
                code: -8,
                message: "txid must be 32 bytes (64 hex chars)".to_string(),
            });
        }
        let mut txid = [0u8; 32];
        txid.copy_from_slice(&txid_bytes);
        let outpoint = OutPoint { txid, vout };

        // Spent outputs return null — do not look further
        if let Some(
            crate::types::UTXOState::SpentFinalized { .. }
            | crate::types::UTXOState::SpentPending { .. }
            | crate::types::UTXOState::Archived { .. },
        ) = self.utxo_manager.get_state(&outpoint)
        {
            return Ok(Value::Null);
        }

        let current_height = self.blockchain.get_height();
        let best_block_hash = hex::encode(
            self.blockchain
                .get_block_hash(current_height)
                .unwrap_or([0u8; 32]),
        );

        // Check mempool first if requested
        if include_mempool {
            if let Some(tx) = self.consensus.tx_pool.get_transaction(&txid) {
                if let Some(output) = tx.outputs.get(vout as usize) {
                    let address = Self::script_pubkey_to_address(&output.script_pubkey).to_string();
                    return Ok(json!({
                        "bestblock": best_block_hash,
                        "confirmations": 0,
                        "value": output.value as f64 / 100_000_000.0,
                        "scriptPubKey": {
                            "hex": hex::encode(&output.script_pubkey),
                            "address": address,
                        },
                        "coinbase": false,
                        "in_mempool": true,
                    }));
                }
            }
        }

        // Look up confirmed UTXO
        match self.utxo_manager.get_utxo(&outpoint).await {
            Ok(utxo) => {
                let confirmations = if let Some(ref tx_index) = self.blockchain.tx_index {
                    if let Some(location) = tx_index.get_location(&txid) {
                        (current_height - location.block_height + 1) as i64
                    } else {
                        1
                    }
                } else {
                    1
                };

                Ok(json!({
                    "bestblock": best_block_hash,
                    "confirmations": confirmations,
                    "value": utxo.value as f64 / 100_000_000.0,
                    "scriptPubKey": {
                        "hex": hex::encode(utxo.address.as_bytes()),
                        "address": utxo.address,
                    },
                    "coinbase": false,
                    "in_mempool": false,
                }))
            }
            Err(_) => Ok(Value::Null),
        }
    }

    /// `testmempoolaccept [rawtxs] [maxfeerate]`
    ///
    /// Validates one or more raw transactions without broadcasting them.
    /// Returns per-tx `allowed` / `reject-reason` the same way Bitcoin Core does.
    async fn test_mempool_accept(&self, params: &[Value]) -> Result<Value, RpcError> {
        let rawtxs = params
            .first()
            .and_then(|v| v.as_array())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected array of raw transactions".to_string(),
            })?;
        let maxfeerate = params.get(1).and_then(|v| v.as_f64()).unwrap_or(0.10); // TIME/kB

        let mut results = Vec::with_capacity(rawtxs.len());

        for raw_tx_val in rawtxs {
            let hex_str = match raw_tx_val.as_str() {
                Some(s) => s,
                None => {
                    results.push(json!({ "allowed": false, "reject-reason": "not a string" }));
                    continue;
                }
            };

            let tx_bytes = match hex::decode(hex_str) {
                Ok(b) => b,
                Err(_) => {
                    results.push(json!({ "allowed": false, "reject-reason": "TX decode failed" }));
                    continue;
                }
            };

            let tx: Transaction = match bincode::deserialize(&tx_bytes) {
                Ok(t) => t,
                Err(_) => {
                    results.push(
                        json!({ "allowed": false, "reject-reason": "TX deserialization failed" }),
                    );
                    continue;
                }
            };

            let txid = hex::encode(tx.txid());

            if tx.inputs.is_empty() {
                results.push(
                    json!({ "txid": txid, "allowed": false, "reject-reason": "bad-txns-vin-empty" }),
                );
                continue;
            }
            if tx.outputs.is_empty() {
                results.push(
                    json!({ "txid": txid, "allowed": false, "reject-reason": "bad-txns-vout-empty" }),
                );
                continue;
            }
            if tx.outputs.iter().any(|o| o.value == 0) {
                results.push(
                    json!({ "txid": txid, "allowed": false, "reject-reason": "bad-txns-vout-toolow" }),
                );
                continue;
            }

            // Accumulate input value and check all inputs are available
            let mut input_sum: u64 = 0;
            let mut reject_reason: Option<&'static str> = None;

            for input in &tx.inputs {
                match self.utxo_manager.get_utxo(&input.previous_output).await {
                    Ok(utxo) => match self.utxo_manager.get_state(&input.previous_output) {
                        Some(
                            crate::types::UTXOState::SpentFinalized { .. }
                            | crate::types::UTXOState::SpentPending { .. }
                            | crate::types::UTXOState::Archived { .. },
                        ) => {
                            reject_reason = Some("bad-txns-inputs-missingorspent");
                            break;
                        }
                        _ => input_sum += utxo.value,
                    },
                    Err(_) => {
                        // Check if it's an output of an unconfirmed mempool tx
                        let in_pool = self
                            .consensus
                            .tx_pool
                            .get_transaction(&input.previous_output.txid)
                            .and_then(|prev| {
                                prev.outputs
                                    .get(input.previous_output.vout as usize)
                                    .map(|o| o.value)
                            });
                        match in_pool {
                            Some(val) => input_sum += val,
                            None => {
                                reject_reason = Some("bad-txns-inputs-missingorspent");
                                break;
                            }
                        }
                    }
                }
            }

            if let Some(reason) = reject_reason {
                results.push(json!({ "txid": txid, "allowed": false, "reject-reason": reason }));
                continue;
            }

            let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
            if output_sum > input_sum {
                results.push(
                    json!({ "txid": txid, "allowed": false, "reject-reason": "bad-txns-in-belowout" }),
                );
                continue;
            }

            let fee = input_sum - output_sum;
            let fee_schedule = self.consensus.current_fee_schedule();
            if fee < fee_schedule.min_fee {
                results.push(json!({
                    "txid": txid,
                    "allowed": false,
                    "reject-reason": format!("min relay fee not met, {} < {}", fee, fee_schedule.min_fee),
                }));
                continue;
            }

            // Fee-rate check (TIME/kB)
            if maxfeerate > 0.0 {
                let fee_rate = (fee as f64 / 100_000_000.0) / (tx_bytes.len() as f64 / 1000.0);
                if fee_rate > maxfeerate {
                    results.push(
                        json!({ "txid": txid, "allowed": false, "reject-reason": "max-fee-exceeded" }),
                    );
                    continue;
                }
            }

            results.push(json!({
                "txid": txid,
                "allowed": true,
                "vsize": tx_bytes.len(),
                "fees": { "base": fee as f64 / 100_000_000.0 },
            }));
        }

        Ok(json!(results))
    }

    /// `estimatesmartfee conf_target [estimate_mode]`
    ///
    /// Returns the recommended fee rate in TIME/kB for a transaction to confirm
    /// within `conf_target` blocks. Because TIME has instant finality (TimeVote),
    /// `conf_target` is informational only — all valid transactions confirm in ≤1 block.
    async fn estimate_smart_fee(&self, params: &[Value]) -> Result<Value, RpcError> {
        let _conf_target = params.first().and_then(|v| v.as_u64()).unwrap_or(6);

        let fee_schedule = self.consensus.current_fee_schedule();
        // Convert min_fee (satoshis per tx, ~250 bytes) to TIME per kB
        let feerate = (fee_schedule.min_fee as f64 / 100_000_000.0) * 4.0;

        Ok(json!({
            "feerate": feerate,
            "blocks": 1,
            "errors": [],
        }))
    }

    /// `getaddressinfo "address"`
    ///
    /// Returns detailed information about an address, including wallet ownership
    /// (`ismine`), associated public key, and validity.  This is the modern
    /// replacement for `validateaddress`, which always returns `ismine: false`.
    async fn get_address_info(&self, params: &[Value]) -> Result<Value, RpcError> {
        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected address".to_string(),
            })?;

        let expected_prefix = match self.network {
            NetworkType::Mainnet => "TIME1",
            NetworkType::Testnet => "TIME0",
        };
        let is_valid = address.starts_with(expected_prefix) && address.len() > 10;

        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address);
        let is_mine = local_address.as_deref() == Some(address);

        let pubkey_hex = self
            .utxo_manager
            .find_pubkey_for_address(address)
            .map(hex::encode)
            .unwrap_or_default();

        Ok(json!({
            "address": address,
            "isvalid": is_valid,
            "scriptPubKey": if is_valid { hex::encode(address.as_bytes()) } else { String::new() },
            "ismine": is_mine,
            "iswatchonly": false,
            "isscript": false,
            "iswitness": false,
            "pubkey": pubkey_hex,
            "iscompressed": true,
            "label": if is_mine { "default" } else { "" },
            "labels": if is_mine {
                json!([{"name": "default", "purpose": "receive"}])
            } else {
                json!([])
            },
        }))
    }

    /// `getconnectioncount`
    ///
    /// Returns the number of currently active masternode connections.
    async fn get_connection_count(&self) -> Result<Value, RpcError> {
        let count = self.registry.count_active().await;
        Ok(json!(count))
    }

    /// `signmessage "address" "message"`
    ///
    /// Sign an arbitrary message with the Ed25519 private key of `address`.
    /// Only the node's own local masternode address can be used (the handler
    /// holds the identity signing key, not an HD keychain).
    ///
    /// Returns a base64-encoded Ed25519 signature. Verify with `verifymessage`.
    async fn sign_message(&self, params: &[Value]) -> Result<Value, RpcError> {
        use ed25519_dalek::Signer;

        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected address".to_string(),
            })?;
        let message = params
            .get(1)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected message".to_string(),
            })?;

        let local_address = self
            .registry
            .get_local_masternode()
            .await
            .map(|mn| mn.reward_address)
            .ok_or_else(|| RpcError {
                code: -4,
                message: "Node is not configured as a masternode".to_string(),
            })?;
        if local_address != address {
            return Err(RpcError {
                code: -4,
                message: "Private key not available for that address".to_string(),
            });
        }

        let signing_key = self.consensus.get_signing_key().ok_or_else(|| RpcError {
            code: -4,
            message: "Signing key not available — node identity not initialised".to_string(),
        })?;

        // Prefix mirrors Bitcoin's approach, substituting "TIME" for "Bitcoin"
        let prefixed = format!("\x18TIME Signed Message:\n{}{}", message.len(), message);
        let signature = signing_key.sign(prefixed.as_bytes());
        let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

        Ok(json!(sig_b64))
    }

    /// `verifymessage "address" "signature" "message"`
    ///
    /// Verify a message signed by `signmessage`. The public key for `address`
    /// is looked up from the on-chain UTXO index, so the address must have
    /// appeared in at least one transaction before it can be verified.
    async fn verify_message(&self, params: &[Value]) -> Result<Value, RpcError> {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let address = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected address".to_string(),
            })?;
        let sig_b64 = params
            .get(1)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected signature (base64)".to_string(),
            })?;
        let message = params
            .get(2)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected message".to_string(),
            })?;

        let pubkey_bytes = self
            .utxo_manager
            .find_pubkey_for_address(address)
            .ok_or_else(|| RpcError {
                code: -5,
                message: format!(
                    "Public key not found for {} — address must have appeared in a transaction",
                    address
                ),
            })?;

        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes).map_err(|_| RpcError {
            code: -5,
            message: "Invalid public key".to_string(),
        })?;

        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(sig_b64)
            .map_err(|_| RpcError {
                code: -5,
                message: "Invalid signature encoding (expected base64)".to_string(),
            })?;
        let sig_arr: [u8; 64] = sig_bytes.try_into().map_err(|_| RpcError {
            code: -5,
            message: "Invalid signature length (expected 64 bytes)".to_string(),
        })?;
        let signature = Signature::from_bytes(&sig_arr);

        let prefixed = format!("\x18TIME Signed Message:\n{}{}", message.len(), message);
        let valid = verifying_key
            .verify(prefixed.as_bytes(), &signature)
            .is_ok();

        Ok(json!(valid))
    }

    /// `lockunspent unlock [{"txid":"...","vout":0}, ...]`
    ///
    /// Bitcoin-compatible UTXO lock/unlock.
    /// `unlock=false` prevents the listed outputs from being selected by the wallet.
    /// `unlock=true`  releases the lock.
    ///
    /// Internally maps to the same `UTXOState::Locked` mechanism used by
    /// `unlockutxo` / `listlockedutxos`, with a sentinel txid (all-zeros) to
    /// distinguish user-initiated locks from in-flight transaction locks.
    async fn lock_unspent(&self, params: &[Value]) -> Result<Value, RpcError> {
        let unlock = params
            .first()
            .and_then(|v| v.as_bool())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Expected unlock (bool)".to_string(),
            })?;

        if let Some(entries) = params.get(1).and_then(|v| v.as_array()) {
            for entry in entries {
                let txid_str =
                    entry
                        .get("txid")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| RpcError {
                            code: -32602,
                            message: "Expected txid in each entry".to_string(),
                        })?;
                let vout = entry
                    .get("vout")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| RpcError {
                        code: -32602,
                        message: "Expected vout in each entry".to_string(),
                    })? as u32;

                let txid_bytes = hex::decode(txid_str).map_err(|_| RpcError {
                    code: -8,
                    message: "Invalid txid encoding".to_string(),
                })?;
                if txid_bytes.len() != 32 {
                    return Err(RpcError {
                        code: -8,
                        message: "txid must be 32 bytes".to_string(),
                    });
                }
                let mut txid = [0u8; 32];
                txid.copy_from_slice(&txid_bytes);
                let outpoint = OutPoint { txid, vout };

                if unlock {
                    if let Some(crate::types::UTXOState::Locked { .. }) =
                        self.utxo_manager.get_state(&outpoint)
                    {
                        self.utxo_manager
                            .update_state(&outpoint, crate::types::UTXOState::Unspent);
                    }
                } else {
                    // Lock with sentinel txid (all-zeros = user-initiated lock)
                    self.utxo_manager.update_state(
                        &outpoint,
                        crate::types::UTXOState::Locked {
                            txid: [0u8; 32],
                            locked_at: chrono::Utc::now().timestamp(),
                        },
                    );
                }
            }
        }

        Ok(json!(true))
    }

    /// `listlockunspent`
    ///
    /// Bitcoin-compatible alias for `listlockedutxos`.
    /// Returns `[{"txid":"...", "vout": N}, ...]` for all currently locked outputs.
    async fn list_lock_unspent(&self) -> Result<Value, RpcError> {
        let locked = self.utxo_manager.get_locked_utxos();
        let result: Vec<Value> = locked
            .iter()
            .map(|(outpoint, _txid, _locked_at)| {
                json!({
                    "txid": hex::encode(outpoint.txid),
                    "vout": outpoint.vout,
                })
            })
            .collect();
        Ok(json!(result))
    }

    /// Store a V4 collateral-claim proof submitted by the wallet so the daemon
    /// can include it in future `MasternodeAnnouncementV4` messages.
    ///
    /// Params: `[txid_hex, vout, proof_hex]`
    /// - `txid_hex`  — collateral UTXO transaction ID (64 hex chars)
    /// - `vout`      — collateral UTXO output index
    /// - `proof_hex` — hex-encoded 64-byte Ed25519 signature over
    ///   `"TIME_COLLATERAL_CLAIM:{txid_hex}:{vout}"`
    async fn submit_collateral_proof(&self, params: &[Value]) -> Result<Value, RpcError> {
        let txid_hex = params
            .first()
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing txid".into(),
            })?;
        let vout: u32 = params
            .get(1)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing vout".into(),
            })? as u32;
        let proof_hex = params
            .get(2)
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcError {
                code: -32602,
                message: "Missing proof".into(),
            })?;

        // Decode txid
        let mut txid_bytes = [0u8; 32];
        hex::decode_to_slice(txid_hex, &mut txid_bytes).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid txid hex".into(),
        })?;

        // Decode proof
        let proof_bytes = hex::decode(proof_hex).map_err(|_| RpcError {
            code: -32602,
            message: "Invalid proof hex".into(),
        })?;
        if proof_bytes.len() != 64 {
            return Err(RpcError {
                code: -32602,
                message: "Proof must be 64 bytes".into(),
            });
        }

        let outpoint = crate::types::OutPoint {
            txid: txid_bytes,
            vout,
        };
        self.registry.store_v4_proof(&outpoint, &proof_bytes);

        tracing::info!(
            "🔑 Stored V4 collateral proof for {}:{} — daemon will use it in next announcement",
            txid_hex,
            vout
        );

        Ok(
            json!({ "success": true, "message": "Proof stored. Daemon will include it in the next MasternodeAnnouncementV4." }),
        )
    }
} // end impl RpcHandler

// ─────────────────────────────────────────────────────────────────────────────
// Free helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Fetch the official peer IP list from `url` (e.g. `https://time-coin.io/api/peers`)
/// and return the parsed set of IP addresses (ports are stripped if present).
///
/// The API is expected to return a JSON array of strings in `"ip"` or `"ip:port"` format.
/// A 10-second timeout is applied; any network or parse error is returned as a `String`.
async fn fetch_official_peer_ips(
    url: &str,
) -> Result<std::collections::HashSet<std::net::IpAddr>, String> {
    // Use curl to fetch peer list (avoids rustls/CDN TLS issues)
    let output = tokio::process::Command::new("curl")
        .args(["-sL", "--max-time", "10", url])
        .output()
        .await
        .map_err(|e| format!("Failed to run curl: {}", e))?;

    if !output.status.success() {
        return Err(format!("curl failed with status {}", output.status));
    }

    let raw: Vec<String> =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("JSON parse error: {}", e))?;

    let mut ips = std::collections::HashSet::new();
    for entry in raw {
        // Strip optional port suffix (handles both "1.2.3.4" and "1.2.3.4:24000")
        let ip_str = if let Some(colon) = entry.rfind(':') {
            let after = &entry[colon + 1..];
            if after.parse::<u16>().is_ok() {
                &entry[..colon]
            } else {
                entry.as_str()
            }
        } else {
            entry.as_str()
        };

        if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
            ips.insert(ip);
        }
    }

    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masternode_registry::MasternodeRegistry;
    use crate::storage::InMemoryUtxoStorage;
    use crate::types::{Masternode, MasternodeTier, OutPoint, UTXO};
    use crate::utxo_manager::UTXOStateManager;
    use ed25519_dalek::SigningKey;

    fn test_outpoint(byte: u8, vout: u32) -> OutPoint {
        OutPoint {
            txid: [byte; 32],
            vout,
        }
    }

    fn test_utxo(outpoint: OutPoint, address: &str, value: u64) -> UTXO {
        UTXO {
            outpoint,
            value,
            script_pubkey: address.as_bytes().to_vec(),
            address: address.to_string(),
            masternode_key: None,
        }
    }

    fn test_masternode(
        address: &str,
        wallet_address: &str,
        outpoint: OutPoint,
        tier: MasternodeTier,
        key_byte: u8,
    ) -> Masternode {
        let signing_key = SigningKey::from_bytes(&[key_byte; 32]);
        Masternode::new_with_collateral(
            address.to_string(),
            wallet_address.to_string(),
            tier.collateral(),
            outpoint,
            signing_key.verifying_key(),
            tier,
            1,
        )
    }

    #[tokio::test]
    async fn reindex_restores_collateral_locks_and_locked_balance() {
        let storage = Arc::new(InMemoryUtxoStorage::new());
        let mut utxo_manager = UTXOStateManager::new_with_storage(storage);
        let db = sled::Config::new().temporary(true).open().unwrap();
        utxo_manager.enable_collateral_persistence(&db).unwrap();
        let utxo_manager = Arc::new(utxo_manager);

        let local_outpoint = test_outpoint(1, 0);
        let remote_outpoint = test_outpoint(2, 0);
        let local_wallet = "time-local-wallet";
        let remote_wallet = "time-remote-wallet";

        utxo_manager
            .add_utxo(test_utxo(
                local_outpoint.clone(),
                local_wallet,
                MasternodeTier::Bronze.collateral(),
            ))
            .await
            .unwrap();
        utxo_manager
            .add_utxo(test_utxo(
                remote_outpoint.clone(),
                remote_wallet,
                MasternodeTier::Bronze.collateral(),
            ))
            .await
            .unwrap();

        let registry_db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let registry = Arc::new(MasternodeRegistry::new(
            registry_db.clone(),
            NetworkType::Testnet,
        ));
        let local_mn = test_masternode(
            "203.0.113.10:24000",
            local_wallet,
            local_outpoint.clone(),
            MasternodeTier::Bronze,
            11,
        );
        let remote_mn = test_masternode(
            "203.0.113.20:24000",
            remote_wallet,
            remote_outpoint.clone(),
            MasternodeTier::Bronze,
            22,
        );

        registry
            .register(local_mn.clone(), local_wallet.to_string())
            .await
            .unwrap();
        registry
            .register(remote_mn.clone(), remote_wallet.to_string())
            .await
            .unwrap();
        registry
            .set_local_masternode(local_mn.address.clone())
            .await;

        let consensus = Arc::new(ConsensusEngine::new(registry.clone(), utxo_manager.clone()));
        let blockchain = Arc::new(crate::blockchain::Blockchain::new(
            db,
            consensus.clone(),
            registry.clone(),
            utxo_manager.clone(),
            NetworkType::Testnet,
        ));
        let handler = RpcHandler::new(
            consensus,
            utxo_manager.clone(),
            NetworkType::Testnet,
            registry,
            blockchain,
            Arc::new(tokio::sync::RwLock::new(
                crate::network::blacklist::IPBlacklist::new(),
            )),
        );

        utxo_manager.clear_all().await.unwrap();
        utxo_manager
            .add_utxo(test_utxo(
                local_outpoint.clone(),
                local_wallet,
                MasternodeTier::Bronze.collateral(),
            ))
            .await
            .unwrap();
        utxo_manager
            .add_utxo(test_utxo(
                remote_outpoint.clone(),
                remote_wallet,
                MasternodeTier::Bronze.collateral(),
            ))
            .await
            .unwrap();

        handler.restore_collateral_locks_after_reindex(42).await;

        assert!(utxo_manager.is_collateral_locked(&local_outpoint));
        assert!(utxo_manager.is_collateral_locked(&remote_outpoint));
        assert_eq!(
            utxo_manager.load_local_collateral_outpoint(),
            Some(local_outpoint.clone())
        );

        let balance = handler.get_balance(&[]).await.unwrap();
        assert_eq!(balance["balance"], json!(1000.0));
        assert_eq!(balance["locked"], json!(1000.0));
        assert_eq!(balance["available"], json!(0.0));
    }
}
