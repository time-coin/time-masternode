use super::context::MessageContext;
use super::MessageHandler;
use crate::network::message::NetworkMessage;
use tracing::debug;

impl MessageHandler {
    pub(super) async fn handle_transaction_broadcast(
        &self,
        tx: crate::types::Transaction,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let txid = tx.txid();

        // Check for duplicates
        if let Some(seen_transactions) = &context.seen_transactions {
            if seen_transactions.check_and_insert(&txid).await {
                debug!(
                    "🔁 [{}] Ignoring duplicate transaction {} from {}",
                    self.direction,
                    hex::encode(&txid[..8]),
                    self.peer_ip
                );
                return Ok(None);
            }
        }

        debug!(
            "📥 [{}] Received transaction {} from {}",
            self.direction,
            hex::encode(&txid[..8]),
            self.peer_ip
        );

        // Record transaction for AI attack detection (double-spend tracking)
        if let Some(ai) = &context.ai_system {
            ai.attack_detector
                .record_transaction(&hex::encode(&txid[..8]), &self.peer_ip);
        }

        // Lock, validate, then process — same path as submit_transaction (not raw process_transaction).
        if let Some(consensus) = &context.consensus {
            if consensus.tx_pool.has_transaction(&txid) {
                debug!(
                    "🔁 [{}] Transaction {} already in pool",
                    self.direction,
                    hex::encode(&txid[..8])
                );
                return Ok(None);
            }

            if consensus.inputs_already_spent_by_other(&tx.inputs, &txid) {
                debug!(
                    "🚫 [{}] Transaction {} rejected: input already spent",
                    self.direction,
                    hex::encode(&txid[..8])
                );
                return Ok(None);
            }

            match consensus.lock_and_validate_transaction(&tx).await {
                Ok(validated_fee) => match consensus
                    .process_transaction(tx.clone(), Some(validated_fee))
                    .await
                {
                    Ok(_) => {
                        debug!(
                            "✅ [{}] Transaction {} processed",
                            self.direction,
                            hex::encode(&txid[..8])
                        );

                        // Gossip to other peers
                        if let Some(broadcast_tx) = &context.broadcast_tx {
                            let msg = NetworkMessage::TransactionBroadcast(tx.clone());
                            if let Ok(receivers) = broadcast_tx.send(msg) {
                                debug!(
                                    "🔄 [{}] Gossiped transaction to {} peer(s)",
                                    self.direction, receivers
                                );
                            }
                        }

                        // Emit WebSocket notification for subscribed wallets
                        if let Some(ref tx_sender) = context.tx_event_sender {
                            let outputs: Vec<crate::rpc::websocket::TxOutputInfo> = tx
                                .outputs
                                .iter()
                                .enumerate()
                                .map(|(i, out)| {
                                    let address = String::from_utf8(out.script_pubkey.clone())
                                        .unwrap_or_else(|_| hex::encode(&out.script_pubkey));
                                    crate::rpc::websocket::TxOutputInfo {
                                        address,
                                        amount: out.value as f64 / 100_000_000.0,
                                        index: i as u32,
                                    }
                                })
                                .collect();

                            let event = crate::rpc::websocket::TransactionEvent {
                                txid: hex::encode(txid),
                                outputs,
                                timestamp: chrono::Utc::now().timestamp(),
                                status: crate::rpc::websocket::TxEventStatus::Pending,
                            };
                            let _ = tx_sender.send(event);
                        }
                    }
                    Err(e) => {
                        debug!(
                            "⚠️ [{}] Transaction {} rejected: {}",
                            self.direction,
                            hex::encode(&txid[..8]),
                            e
                        );
                    }
                },
                Err(e) => {
                    debug!(
                        "🚫 [{}] Transaction {} failed lock/validation: {}",
                        self.direction,
                        hex::encode(&txid[..8]),
                        e
                    );
                }
            }
        } else {
            debug!(
                "⚠️ [{}] No consensus engine to process transaction",
                self.direction
            );
        }

        Ok(None)
    }

    pub(super) async fn handle_transaction_finalized(
        &self,
        txid: [u8; 32],
        tx: crate::types::Transaction,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Skip while syncing — UTXOs not yet indexed
        if context.blockchain.is_syncing() {
            tracing::debug!(
                "⏸️ Skipping TransactionFinalized {} — node is syncing",
                hex::encode(txid)
            );
            return Ok(None);
        }

        // Dedup: skip if we've already processed this finalization
        if let Some(seen_tx_finalized) = &context.seen_tx_finalized {
            if seen_tx_finalized.check_and_insert(&txid).await {
                tracing::debug!(
                    "🔁 Ignoring duplicate TransactionFinalized {} from {}",
                    hex::encode(txid),
                    self.peer_ip
                );
                return Ok(None);
            }
        }

        // Drop TXs already committed to the chain — re-gossip of archived TXs must not
        // re-populate the confirmed pool, which would cause them to persist indefinitely
        // across restarts (the persisted mempool reloads them on every startup).
        if context.blockchain.is_tx_archived(&txid) {
            tracing::debug!(
                "⛓️ TransactionFinalized {} from {} dropped — already in chain",
                hex::encode(txid),
                self.peer_ip
            );
            return Ok(None);
        }

        tracing::info!(
            "✅ Transaction {} finalized (from {})",
            hex::encode(txid),
            self.peer_ip
        );

        // AV38+AV40: drop null TXs (0 inputs, 0 outputs, no special_data)
        if tx.inputs.is_empty() && tx.outputs.is_empty() && tx.special_data.is_none() {
            tracing::debug!(
                "🗑️ Null TX {} via TransactionFinalized from {} — dropped (AV38+AV40)",
                hex::encode(txid),
                self.peer_ip
            );
            if let Some(ref ai) = context.ai_system {
                ai.attack_detector.record_finality_injection(&self.peer_ip);
            }
            return Ok(None);
        }

        // AV41: ghost special_data guard — inputs/outputs empty but special_data present
        if tx.inputs.is_empty() && tx.outputs.is_empty() {
            let sig_ok = tx.special_data.as_ref().is_some_and(|sd| {
                sd.validate_fields().is_ok()
                    && sd.verify_signature().is_ok()
                    && sd.verify_address_binding().is_ok()
            });
            if !sig_ok {
                tracing::debug!(
                    "🗑️ Ghost/forged special_data TX {} via TransactionFinalized from {} — dropped (AV41)",
                    hex::encode(txid),
                    self.peer_ip
                );
                if let Some(ref ai) = context.ai_system {
                    ai.attack_detector.record_finality_injection(&self.peer_ip);
                }
                return Ok(None);
            }
        }

        // Block new v1 value-transfer TXs from entering the finalized pool via gossip.
        // Legacy v1 TXs already in the pool are handled by produce_block_at_height.
        if !tx.inputs.is_empty() && tx.version < 2 {
            tracing::debug!(
                "🚫 TransactionFinalized {} from {} dropped — v1 TXs no longer accepted (upgrade wallet)",
                hex::encode(txid),
                self.peer_ip
            );
            return Ok(None);
        }

        let consensus = match &context.consensus {
            Some(c) => c,
            None => return Ok(None),
        };

        // Already finalized — skip silently; the seen_tx_finalized dedup filter ensures
        // we already gossiped this txid exactly once, so re-gossiping here would
        // create an O(N²) broadcast storm identical to the vote relay loop (AV-relay-loop).
        if consensus.tx_pool.is_finalized(&txid) {
            tracing::debug!(
                "📪 TX {} already in finalized pool, skipping re-gossip",
                hex::encode(txid)
            );
            return Ok(None);
        }

        // Check whether all input UTXOs are accounted for locally.
        // Tombstoned inputs are legitimately spent (removed from sled storage by
        // mark_timevote_finalized) and must NOT be treated as a UTXO-set divergence.
        let mut inputs_exist = true;
        for input in &tx.inputs {
            let in_storage = consensus
                .utxo_manager
                .get_utxo(&input.previous_output)
                .await
                .is_ok();
            let tombstoned = consensus.utxo_manager.is_tombstoned(&input.previous_output);
            if !in_storage && !tombstoned {
                tracing::warn!(
                    "⚠️ TransactionFinalized {} from {}: input {} not in local storage \
                     and not tombstoned (UTXO set diverged) — will apply outputs without \
                     marking inputs spent",
                    hex::encode(txid),
                    self.peer_ip,
                    input.previous_output
                );
                inputs_exist = false;
                break;
            }
        }

        if !inputs_exist {
            // Apply outputs directly so the recipient wallet sees the new UTXOs
            // even while our local set is diverged.
            for (idx, output) in tx.outputs.iter().enumerate() {
                let outpoint = crate::types::OutPoint {
                    txid,
                    vout: idx as u32,
                };
                let utxo = crate::types::UTXO {
                    outpoint: outpoint.clone(),
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address: String::from_utf8(output.script_pubkey.clone()).unwrap_or_default(),
                    masternode_key: None,
                };
                if let Err(e) = consensus.utxo_manager.add_utxo(utxo).await {
                    tracing::warn!(
                        "Failed to add output UTXO vout={} for diverged TX {}: {}",
                        idx,
                        hex::encode(txid),
                        e
                    );
                } else {
                    consensus
                        .utxo_manager
                        .update_state(&outpoint, crate::types::UTXOState::Unspent);
                }
            }
            if let Some(ref broadcast_tx) = context.broadcast_tx {
                let _ = broadcast_tx.send(
                    crate::network::message::NetworkMessage::TransactionFinalized {
                        txid,
                        tx: tx.clone(),
                    },
                );
            }
            return Ok(None);
        }

        if consensus.inputs_already_spent_by_other(&tx.inputs, &txid) {
            tracing::warn!(
                "🚫 TransactionFinalized {} from {} dropped — input already spent by another TX",
                hex::encode(txid),
                self.peer_ip
            );
            if let Some(ref ai) = context.ai_system {
                ai.attack_detector.record_finality_injection(&self.peer_ip);
            }
            return Ok(None);
        }

        if consensus.has_double_spend_conflict(&tx.inputs, &txid) {
            tracing::warn!(
                "🚫 TransactionFinalized {} from {} dropped — competes with another in-pool TX",
                hex::encode(txid),
                self.peer_ip
            );
            if let Some(ref ai) = context.ai_system {
                ai.attack_detector.record_finality_injection(&self.peer_ip);
            }
            return Ok(None);
        }

        // Add to pool if not present — lock/validate first (AV38: never call process_transaction).
        if !consensus.tx_pool.has_transaction(&txid) {
            tracing::warn!(
                "⚠️ TransactionFinalized for unknown TX {} from {} — \
                 validating before pool add (AV38 guard)",
                hex::encode(txid),
                self.peer_ip
            );
            if let Some(ref ai) = context.ai_system {
                ai.attack_detector.record_finality_injection(&self.peer_ip);
            }
            let validated_fee = match consensus.lock_and_validate_transaction(&tx).await {
                Ok(fee) => fee,
                Err(e) => {
                    tracing::warn!(
                        "🚫 TransactionFinalized {} from {} dropped — validation failed: {}",
                        hex::encode(txid),
                        self.peer_ip,
                        e
                    );
                    return Ok(None);
                }
            };
            if let Err(e) = consensus.tx_pool.add_pending(tx.clone(), validated_fee) {
                tracing::debug!(
                    "TransactionFinalized {} pool add skipped: {}",
                    hex::encode(txid),
                    e
                );
            }
        } else if let Err(e) = consensus.validate_transaction(&tx).await {
            tracing::warn!(
                "🚫 TransactionFinalized {} from {} dropped — re-validation failed: {}",
                hex::encode(txid),
                self.peer_ip,
                e
            );
            return Ok(None);
        }

        // Manual finalization: move TX to finalized pool and update UTXO states
        if consensus.tx_pool.finalize_transaction(txid) {
            tracing::info!(
                "📪 Moved TX {} to finalized pool on this node",
                hex::encode(txid)
            );

            // Transition input UTXOs → SpentFinalized (removes from sled + address_index)
            for input in &tx.inputs {
                consensus
                    .utxo_manager
                    .mark_timevote_finalized(&input.previous_output, txid)
                    .await;
            }

            // Create output UTXOs
            for (idx, output) in tx.outputs.iter().enumerate() {
                let outpoint = crate::types::OutPoint {
                    txid,
                    vout: idx as u32,
                };
                let utxo = crate::types::UTXO {
                    outpoint: outpoint.clone(),
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address: String::from_utf8(output.script_pubkey.clone()).unwrap_or_default(),
                    masternode_key: None,
                };
                if let Err(e) = consensus.utxo_manager.add_utxo(utxo).await {
                    tracing::warn!("Failed to add output UTXO vout={}: {}", idx, e);
                }
                consensus
                    .utxo_manager
                    .update_state(&outpoint, crate::types::UTXOState::Unspent);
            }
        } else {
            tracing::debug!(
                "⚠️ Could not finalize TX {} (not in pending pool)",
                hex::encode(txid)
            );
        }

        // Signal WS subscribers
        consensus.signal_tx_finalized(txid);

        // Gossip finalization to other peers
        if let Some(ref broadcast_tx) = context.broadcast_tx {
            match broadcast_tx.send(
                crate::network::message::NetworkMessage::TransactionFinalized {
                    txid,
                    tx: tx.clone(),
                },
            ) {
                Ok(receivers) => tracing::debug!(
                    "🔄 Gossiped finalization to {} peer(s)",
                    receivers.saturating_sub(1)
                ),
                Err(e) => tracing::debug!("Failed to gossip finalization: {}", e),
            }
        }

        Ok(None)
    }

    /// Serve a `MempoolSyncRequest` — respond with the full local mempool state so the
    /// connecting peer can bootstrap its pending and finalized pools.
    pub(super) async fn handle_mempool_sync_request(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let all_entries = if let Some(consensus) = &context.consensus {
            consensus.get_all_for_sync()
        } else {
            Vec::new()
        };

        // Cap at 100 finalized + 100 pending to prevent large single-frame responses.
        // The server.rs inbound handler uses the same limit. Without this cap, nodes
        // with large mempools generate MempoolSyncResponse frames of 4MB+ per connection.
        let mut entries: Vec<_> = all_entries
            .iter()
            .filter(|e| e.is_finalized)
            .take(100)
            .cloned()
            .collect();
        entries.extend(
            all_entries
                .iter()
                .filter(|e| !e.is_finalized)
                .take(100)
                .cloned(),
        );

        tracing::debug!(
            "📤 [{}] Serving mempool sync to {}: {} entries ({} finalized)",
            self.direction,
            self.peer_ip,
            entries.len(),
            entries.iter().filter(|e| e.is_finalized).count(),
        );

        Ok(Some(NetworkMessage::MempoolSyncResponse(entries)))
    }

    /// Handle a `MempoolSyncResponse` received from a peer on connect.
    /// Pending entries are processed through the normal consensus path (starts TimeVote).
    /// Finalized entries are added directly to the finalized pool to preserve their status.
    pub(super) async fn handle_mempool_sync_response(
        &self,
        entries: Vec<crate::network::message::MempoolSyncEntry>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        if entries.is_empty() {
            return Ok(None);
        }

        let pending_count = entries.iter().filter(|e| !e.is_finalized).count();
        let finalized_count = entries.iter().filter(|e| e.is_finalized).count();

        tracing::info!(
            "📥 [{}] Mempool sync from {}: {} pending + {} finalized transaction(s)",
            self.direction,
            self.peer_ip,
            pending_count,
            finalized_count,
        );

        if let Some(consensus) = &context.consensus {
            let mut added_pending = 0usize;
            let mut added_finalized = 0usize;

            for entry in entries {
                let txid = entry.tx.txid();

                if consensus.tx_pool.has_transaction(&txid) {
                    continue;
                }

                // Drop TXs already committed to the chain — prevents archived TXs from
                // re-entering the pool via mempool sync on every peer connection.
                if context.blockchain.is_tx_archived(&txid) {
                    tracing::debug!(
                        "⛓️ MempoolSync TX {} from {} dropped — already in chain",
                        hex::encode(txid),
                        self.peer_ip
                    );
                    continue;
                }

                // Block new v1 value-transfer TXs from entering the pool via mempool sync.
                if !entry.tx.inputs.is_empty() && entry.tx.version < 2 {
                    tracing::debug!(
                        "🚫 MempoolSync TX {} from {} dropped — v1 TXs no longer accepted",
                        hex::encode(txid),
                        self.peer_ip
                    );
                    continue;
                }

                if entry.is_finalized {
                    // AV47: Validate ghost TXs before accepting from mempool sync.
                    // A peer with ghost TXs in its finalized pool will send them here.
                    // Without this check they bypass the TransactionFinalized guard entirely.
                    if entry.tx.inputs.is_empty() && entry.tx.outputs.is_empty() {
                        let ok = entry.tx.special_data.as_ref().is_some_and(|sd| {
                            sd.validate_fields().is_ok()
                                && sd.verify_signature().is_ok()
                                && sd.verify_address_binding().is_ok()
                        });
                        if !ok {
                            tracing::debug!(
                                "🗑️ [AV47] Ghost TX in mempool sync from {} — dropped",
                                self.peer_ip
                            );
                            continue;
                        }
                    }
                    // Add to the finalized pool.  add_finalized_direct only updates the pool;
                    // it does NOT update UTXO state.  We must do that here so that:
                    //   (a) block producers see inputs as SpentFinalized (valid for assembly)
                    //   (b) subsequent TransactionFinalized broadcasts don't skip UTXO updates
                    //       because is_finalized() already returns true (server.rs:1793)
                    let tx = entry.tx.clone();
                    consensus.add_finalized_direct(entry.tx, entry.fee);
                    // Update UTXO state: inputs → SpentFinalized, outputs → Unspent
                    if let Some(utxo_manager) = &context.utxo_manager {
                        for input in &tx.inputs {
                            utxo_manager
                                .mark_timevote_finalized(&input.previous_output, txid)
                                .await;
                        }
                        for (idx, output) in tx.outputs.iter().enumerate() {
                            let outpoint = crate::types::OutPoint {
                                txid,
                                vout: idx as u32,
                            };
                            let utxo = crate::types::UTXO {
                                outpoint: outpoint.clone(),
                                value: output.value,
                                script_pubkey: output.script_pubkey.clone(),
                                address: String::from_utf8(output.script_pubkey.clone())
                                    .unwrap_or_default(),
                                masternode_key: None,
                            };
                            if let Err(e) = utxo_manager.add_utxo(utxo).await {
                                tracing::debug!(
                                    "UTXO sync: output vout={} for TX {} already exists: {}",
                                    idx,
                                    hex::encode(txid),
                                    e
                                );
                            } else {
                                utxo_manager
                                    .update_state(&outpoint, crate::types::UTXOState::Unspent);
                            }
                        }
                    }
                    added_finalized += 1;
                } else {
                    // Route through consensus so TimeVote starts for this TX.
                    // Ignore errors (duplicate, pool full, etc.).
                    let _ = consensus
                        .process_transaction(entry.tx, Some(entry.fee))
                        .await;
                    added_pending += 1;
                }
            }

            tracing::info!(
                "✅ Mempool sync from {} complete: +{} pending, +{} finalized",
                self.peer_ip,
                added_pending,
                added_finalized,
            );
        }

        Ok(None)
    }
}
