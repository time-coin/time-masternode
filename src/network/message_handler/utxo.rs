use super::common::*;
use super::context::MessageContext;
use super::MessageHandler;
use crate::network::message::NetworkMessage;
use crate::types::{OutPoint, UTXOState};
use std::time::Instant;
use tracing::{debug, error, info, warn};

impl MessageHandler {
    pub(super) async fn handle_utxo_state_query(
        &self,
        outpoints: Vec<crate::types::OutPoint>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received UTXOStateQuery for {} outpoints from {}",
            self.direction,
            outpoints.len(),
            self.peer_ip
        );

        if let Some(utxo_manager) = &context.utxo_manager {
            let mut responses = Vec::new();
            for op in &outpoints {
                if let Some(state) = utxo_manager.get_state(op) {
                    responses.push((op.clone(), state));
                }
            }
            Ok(Some(NetworkMessage::UTXOStateResponse(responses)))
        } else {
            debug!(
                "⚠️ [{}] No UTXO manager to handle state query",
                self.direction
            );
            Ok(None)
        }
    }

    /// Handle GetUTXOStateHash
    pub(super) async fn handle_get_utxo_state_hash(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        let utxo_hash = context.blockchain.get_utxo_state_hash().await;
        let utxo_count = context.blockchain.get_utxo_count().await;

        debug!(
            "📤 [{}] Sending UTXO state hash to {}",
            self.direction, self.peer_ip
        );
        Ok(Some(NetworkMessage::UTXOStateHashResponse {
            hash: utxo_hash,
            height,
            utxo_count,
        }))
    }

    /// Handle GetUTXOSet — stream the full UTXO set, chunked if it exceeds the
    /// 8 MiB frame limit.  When the set fits in one frame the legacy
    /// `UTXOSetResponse` type is used so that older nodes still work.
    pub(super) async fn handle_get_utxo_set(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let utxos = context.blockchain.get_all_utxos().await;
        let utxo_count = utxos.len();
        let chunks = split_utxos_into_chunks(utxos);
        let total = chunks.len() as u32;

        if total == 1 {
            info!(
                "📤 [{}] Sending UTXO set ({} UTXOs) to {}",
                self.direction, utxo_count, self.peer_ip
            );
            return Ok(Some(NetworkMessage::UTXOSetResponse(
                chunks.into_iter().next().unwrap_or_default(),
            )));
        }

        info!(
            "📤 [{}] UTXO set too large for one frame ({} UTXOs → {} chunks) — streaming to {}",
            self.direction, utxo_count, total, self.peer_ip
        );

        // Stream all chunks except the last directly; return the last one as the
        // normal handler response so the caller's send path is used for it.
        // yield_now() between chunks lets other tasks run and prevents the entire
        // burst from landing in the same tokio scheduler tick at the receiver.
        for (i, chunk) in chunks.iter().enumerate().take((total - 1) as usize) {
            let msg = NetworkMessage::UTXOSetChunk {
                index: i as u32,
                total,
                utxos: chunk.clone(),
            };
            let _ = context.peer_registry.send_to_peer(&self.peer_ip, msg).await;
            tokio::task::yield_now().await;
        }

        let last_chunk = chunks.into_iter().last().unwrap_or_default();
        Ok(Some(NetworkMessage::UTXOSetChunk {
            index: total - 1,
            total,
            utxos: last_chunk,
        }))
    }

    /// Handle UTXOStateHashResponse — compare peer's UTXO hash with ours.
    /// Caches the peer's hash and checks for 2/3 supermajority consensus.
    /// If 2/3+ of voters report a different hash at the same height, requests
    /// the full UTXO set from a majority peer for reconciliation.
    /// Fully event-driven: re-evaluates on every response received.
    pub(super) async fn handle_utxo_state_hash_response(
        &self,
        peer_hash: [u8; 32],
        peer_height: u64,
        peer_utxo_count: usize,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_height = context.blockchain.get_height();
        let our_hash = context.blockchain.get_utxo_state_hash().await;
        let our_count = context.blockchain.get_utxo_count().await;

        // Always cache the peer's response
        peer_utxo_hash_cache().insert(
            self.peer_ip.clone(),
            PeerUtxoHashEntry {
                hash: peer_hash,
                height: peer_height,
                _utxo_count: peer_utxo_count,
                received_at: Instant::now(),
            },
        );

        if our_height != peer_height {
            debug!(
                "🔄 [{}] UTXO hash from {} at height {} (we're at {}) — skipping (height mismatch)",
                self.direction, self.peer_ip, peer_height, our_height
            );
            return Ok(None);
        }

        if peer_hash == our_hash {
            debug!(
                "✅ [{}] UTXO state matches {} at height {} ({} UTXOs, hash {})",
                self.direction,
                self.peer_ip,
                our_height,
                our_count,
                hex::encode(&our_hash[..8])
            );
            // Reset divergence counter — we match this peer
            utxo_divergence_rounds().store(0, std::sync::atomic::Ordering::Relaxed);
            return Ok(None);
        }

        // Divergence detected — tally ALL cached votes at this height
        warn!(
            "⚠️  [{}] UTXO DIVERGENCE with {} at height {}: ours {}({} utxos) vs theirs {}({} utxos)",
            self.direction,
            self.peer_ip,
            our_height,
            hex::encode(&our_hash[..8]),
            our_count,
            hex::encode(&peer_hash[..8]),
            peer_utxo_count,
        );

        let now = Instant::now();
        let mut our_hash_votes = 1u32; // Count ourselves
        let mut hash_counts: std::collections::HashMap<[u8; 32], (u32, String)> =
            std::collections::HashMap::new();

        for entry in peer_utxo_hash_cache().iter() {
            if now.duration_since(entry.received_at) > UTXO_HASH_CACHE_TTL {
                continue;
            }
            if entry.height != our_height {
                continue;
            }
            if entry.hash == our_hash {
                our_hash_votes += 1;
            } else {
                let counter = hash_counts
                    .entry(entry.hash)
                    .or_insert((0, entry.key().clone()));
                counter.0 += 1;
            }
        }

        // Find the most popular alternative hash
        let mut best_alt_votes = 0u32;
        let mut best_alt_peer: Option<String> = None;
        let mut best_alt_hash: Option<[u8; 32]> = None;
        for (hash, (count, peer)) in &hash_counts {
            if *count > best_alt_votes {
                best_alt_votes = *count;
                best_alt_peer = Some(peer.clone());
                best_alt_hash = Some(*hash);
            }
        }

        let total_votes = our_hash_votes + hash_counts.values().map(|(c, _)| c).sum::<u32>();

        // Liveness-adjusted threshold:
        //   Round 0 (first divergence): 2/3 supermajority required
        //   Round 1 (still diverged):   simple majority (>50%)
        //   Round 2+ (persistent):      plurality (largest group wins)
        let rounds = utxo_divergence_rounds().load(std::sync::atomic::Ordering::Relaxed);
        let (threshold_name, should_reconcile) = if rounds == 0 {
            // 2/3 supermajority: alt_votes * 3 >= total * 2
            ("2/3 supermajority", best_alt_votes * 3 >= total_votes * 2)
        } else if rounds == 1 {
            // Simple majority: alt_votes > total / 2
            ("simple majority", best_alt_votes * 2 > total_votes)
        } else {
            // Plurality: alt has more votes than us, or tied with lowest hash winning.
            // Lowest-hash tiebreaker ensures deterministic resolution in 2-node networks.
            let tied_and_lower = best_alt_votes == our_hash_votes
                && best_alt_votes > 0
                && best_alt_hash.is_some_and(|alt| alt < our_hash);
            (
                "plurality",
                best_alt_votes > our_hash_votes || tied_and_lower,
            )
        };

        info!(
            "🗳️  [{}] UTXO hash votes at height {}: ours={}, best_alt={}, total={}, threshold={} (round {})",
            self.direction, our_height, our_hash_votes, best_alt_votes, total_votes,
            threshold_name, rounds,
        );

        if should_reconcile {
            if let Some(alt_peer) = best_alt_peer {
                // Only request from peers that support chunked UTXO transfer.
                // Old-code peers respond with a single massive frame that exceeds
                // MAX_FRAME_SIZE and corrupts the TCP stream (frame-bomb loop).
                let peer_commit = context
                    .peer_registry
                    .get_peer_commit_count(&self.peer_ip)
                    .await
                    .unwrap_or(0);
                if peer_commit < crate::constants::MIN_UTXO_CHUNK_COMMIT {
                    warn!(
                        "⚠️ [{}] Skipping GetUTXOSet to {} — pre-chunking code (commit {}, need ≥{}). \
                        Will reconcile via a chunk-capable peer when one connects.",
                        self.direction, self.peer_ip, peer_commit,
                        crate::constants::MIN_UTXO_CHUNK_COMMIT
                    );
                    return Ok(None);
                }
                warn!(
                    "📥 [{}] We are in the MINORITY ({}/{} votes, threshold={}, round {}) — requesting UTXO set from {} for reconciliation",
                    self.direction, our_hash_votes, total_votes,
                    threshold_name, rounds, alt_peer
                );
                return Ok(Some(NetworkMessage::GetUTXOSet));
            }
        }

        // Still diverged but threshold not met — increment round for next check
        utxo_divergence_rounds().fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        info!(
            "📊 [{}] Threshold not met ({}) — relaxing for next round ({}→{})",
            self.direction,
            threshold_name,
            rounds,
            rounds + 1
        );
        Ok(None)
    }

    /// Handle UTXOSetResponse — diff against our local set and reconcile.
    /// This is only requested when we've already determined we're in the minority,
    /// so we proceed with reconciliation.
    pub(super) async fn handle_utxo_set_response(
        &self,
        remote_utxos: Vec<crate::types::UTXO>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let utxo_mgr = &context.blockchain.utxo_manager;
        let (to_remove, to_add) = utxo_mgr.get_utxo_diff(&remote_utxos).await;

        if to_remove.is_empty() && to_add.is_empty() {
            // Reconciliation succeeded — reset divergence counter
            utxo_divergence_rounds().store(0, std::sync::atomic::Ordering::Relaxed);
            info!(
                "✅ [{}] UTXO set from {} matches — no diff",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        info!(
            "🔧 [{}] Reconciling UTXO set from {} ({} removals, {} additions)",
            self.direction,
            self.peer_ip,
            to_remove.len(),
            to_add.len()
        );

        if let Err(e) = utxo_mgr.reconcile_utxo_state(to_remove, to_add).await {
            error!("❌ [{}] UTXO reconciliation failed: {}", self.direction, e);
        } else {
            // Reconciliation succeeded — reset divergence counter
            utxo_divergence_rounds().store(0, std::sync::atomic::Ordering::Relaxed);
            let new_hash = context.blockchain.get_utxo_state_hash().await;
            info!(
                "✅ [{}] UTXO reconciliation complete. New state hash: {}",
                self.direction,
                hex::encode(&new_hash[..8])
            );
        }

        // After UTXO set reconciliation, also sync states for UTXOs in any
        // intermediate state. The UTXO diff only detects existence changes; two nodes
        // can agree on the exact same UTXO set while disagreeing on state:
        //   - Unspent on us vs SpentFinalized on peer: balance discrepancy
        //   - Locked on us vs SpentFinalized on peer: TX stuck in pending forever
        //   - SpentPending on us vs SpentFinalized on peer: stale vote state
        // Querying the peer for all non-Archived states lets us advance stale entries.
        let in_flight_outpoints: Vec<crate::types::OutPoint> = utxo_mgr
            .utxo_states
            .iter()
            .filter(|e| {
                matches!(
                    e.value(),
                    crate::types::UTXOState::Unspent
                        | crate::types::UTXOState::Locked { .. }
                        | crate::types::UTXOState::SpentPending { .. }
                )
            })
            .map(|e| e.key().clone())
            .collect();

        if !in_flight_outpoints.is_empty() {
            debug!(
                "🔍 [{}] Querying {} in-flight UTXO states from {} for cross-node sync",
                self.direction,
                in_flight_outpoints.len(),
                self.peer_ip
            );
            return Ok(Some(NetworkMessage::UTXOStateQuery(in_flight_outpoints)));
        }

        Ok(None)
    }

    /// Handle a UTXOSetChunk (one page of a multi-frame GetUTXOSet response).
    /// Accumulates chunks until the last one arrives, then runs the full diff/reconcile.
    pub(super) async fn handle_utxo_set_chunk(
        &self,
        index: u32,
        total: u32,
        chunk: Vec<crate::types::UTXO>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let chunk_len = chunk.len();
        utxo_set_chunk_buf()
            .entry(self.peer_ip.clone())
            .or_default()
            .extend(chunk);

        if index + 1 < total {
            debug!(
                "📥 [{}] UTXOSetChunk {}/{} from {} ({} UTXOs in this chunk)",
                self.direction,
                index + 1,
                total,
                self.peer_ip,
                chunk_len
            );
            return Ok(None);
        }

        // Final chunk — hand the assembled set to the normal response handler.
        let all_utxos = utxo_set_chunk_buf()
            .remove(&self.peer_ip)
            .map(|(_, v)| v)
            .unwrap_or_default();
        info!(
            "📥 [{}] UTXOSetChunk complete from {} ({} total UTXOs, {} chunks) — reconciling",
            self.direction,
            self.peer_ip,
            all_utxos.len(),
            total
        );
        self.handle_utxo_set_response(all_utxos, context).await
    }

    /// Handle a UtxoReconciliationChunk (one page of a multi-frame
    /// RequestUtxoReconciliation response).
    /// Accumulates until the last chunk, then applies the full set.
    pub(super) async fn handle_utxo_reconciliation_chunk(
        &self,
        at_height: u64,
        index: u32,
        total: u32,
        chunk: Vec<crate::types::UTXO>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let chunk_len = chunk.len();
        utxo_reconcil_chunk_buf()
            .entry(self.peer_ip.clone())
            .or_insert_with(|| (at_height, Vec::new()))
            .1
            .extend(chunk);

        if index + 1 < total {
            debug!(
                "📥 [{}] UtxoReconciliationChunk {}/{} from {} ({} UTXOs)",
                self.direction,
                index + 1,
                total,
                self.peer_ip,
                chunk_len
            );
            return Ok(None);
        }

        let (stored_height, all_utxos) = utxo_reconcil_chunk_buf()
            .remove(&self.peer_ip)
            .map(|(_, v)| v)
            .unwrap_or((at_height, Vec::new()));

        info!(
            "[{}] UtxoReconciliationChunk complete from {} — {} UTXOs at height {}. Applying…",
            self.direction,
            self.peer_ip,
            all_utxos.len(),
            stored_height
        );
        let mut applied = 0usize;
        for utxo in all_utxos {
            let outpoint = utxo.outpoint.clone();
            if context
                .blockchain
                .utxo_manager
                .get_utxo(&outpoint)
                .await
                .is_err()
                && context
                    .blockchain
                    .utxo_manager
                    .add_utxo(utxo.clone())
                    .await
                    .is_ok()
            {
                applied += 1;
            }
        }
        info!(
            "[{}] UTXO reconciliation complete — applied {} new UTXOs from {}. \
             Node will re-enter voting on next round.",
            self.direction, applied, self.peer_ip
        );
        Ok(None)
    }

    /// Handle UTXOStateResponse — apply state updates received from a majority peer.
    /// Only advances states forward (never reverts spent → unspent) to prevent
    /// a malicious peer from fabricating spendable UTXOs.
    pub(super) async fn handle_utxo_state_response(
        &self,
        remote_states: Vec<(crate::types::OutPoint, crate::types::UTXOState)>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        if remote_states.is_empty() {
            return Ok(None);
        }
        let utxo_mgr = &context.blockchain.utxo_manager;
        debug!(
            "📥 [{}] Received UTXOStateResponse ({} entries) from {}",
            self.direction,
            remote_states.len(),
            self.peer_ip
        );
        utxo_mgr.apply_state_updates(remote_states);
        Ok(None)
    }

    /// Handle ConsensusQuery
    pub(super) async fn handle_utxo_state_update(
        &self,
        outpoint: OutPoint,
        state: UTXOState,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Dedup: skip if we've already processed this exact UTXO state update
        let mut lock_id = Vec::new();
        lock_id.extend_from_slice(&outpoint.txid);
        lock_id.extend_from_slice(&outpoint.vout.to_le_bytes());
        match &state {
            UTXOState::Locked { txid, .. } => {
                lock_id.push(1);
                lock_id.extend_from_slice(txid);
            }
            UTXOState::Unspent => lock_id.push(2),
            UTXOState::SpentPending { txid, .. } => {
                lock_id.push(3);
                lock_id.extend_from_slice(txid);
            }
            UTXOState::SpentFinalized { txid, .. } => {
                lock_id.push(4);
                lock_id.extend_from_slice(txid);
            }
            UTXOState::Archived { txid, .. } => {
                lock_id.push(5);
                lock_id.extend_from_slice(txid);
            }
        }
        if let Some(seen_utxo_locks) = &context.seen_utxo_locks {
            if seen_utxo_locks.check_and_insert(&lock_id).await {
                return Ok(None);
            }
        }
        tracing::debug!(
            "🔒 [{}] Received UTXO state update for {} -> {:?}",
            self.direction,
            outpoint,
            state
        );

        // Apply peer lock gossip only — never accept spend/unlock state from remote nodes.
        if let Some(consensus) = &context.consensus {
            match state {
                UTXOState::Locked { txid, .. } => {
                    match consensus.utxo_manager.lock_utxo(&outpoint, txid) {
                        Ok(()) => {
                            tracing::debug!(
                                "🔒 [{}] Locked UTXO {} for TX {} (peer gossip)",
                                self.direction,
                                outpoint,
                                hex::encode(txid)
                            );
                        }
                        Err(e) => {
                            tracing::debug!(
                                "🚫 [{}] Rejected peer lock on {} for TX {}: {}",
                                self.direction,
                                outpoint,
                                hex::encode(txid),
                                e
                            );
                        }
                    }
                }
                UTXOState::Unspent
                | UTXOState::SpentPending { .. }
                | UTXOState::SpentFinalized { .. }
                | UTXOState::Archived { .. } => {
                    tracing::debug!(
                        "🚫 [{}] Rejected peer UTXO state update {} -> {:?}",
                        self.direction,
                        outpoint,
                        state
                    );
                }
            }
        }

        Ok(None)
    }

    // === TimeVote Consensus Handlers ===
}
