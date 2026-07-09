use super::common::*;
use super::context::MessageContext;
use super::MessageHandler;
use crate::block::types::calculate_merkle_root;
use crate::block::types::Block;
use crate::network::message::NetworkMessage;
use std::time::Instant;
use tracing::{debug, info, warn};

impl MessageHandler {
    pub(super) async fn handle_timelock_block_proposal(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_height = block.header.height;

        info!(
            "📦 [{}] Received TimeLock Block proposal at height {} from {}",
            self.direction, block_height, self.peer_ip
        );

        // Validate: Only accept proposals for the next block (current + 1)
        let our_height = context.blockchain.get_height();
        let expected_height = our_height + 1;

        if block_height != expected_height {
            debug!(
                "⏭️ [{}] Rejecting block proposal at height {} (expected {})",
                self.direction, block_height, expected_height
            );
            return Ok(None);
        }

        // CRITICAL SECURITY: Validate block BEFORE voting
        // This prevents voting on blocks with invalid transactions, UTXOs, or signatures
        if let Err(e) = self.validate_block_before_vote(&block, context).await {
            warn!(
                "❌ [{}] Rejecting invalid block at height {} from {}: {}",
                self.direction, block_height, self.peer_ip, e
            );
            return Ok(None);
        }

        // VRF best-proposal selection: if we already have a proposal at this height,
        // only accept this one if it has a lower (better) VRF score
        let mut switching_vote = false;
        if let Some(cache) = &context.block_cache {
            if let Some(existing) = cache.get_by_height(block_height) {
                if existing.header.vrf_score > 0 && block.header.vrf_score > 0 {
                    if block.header.vrf_score >= existing.header.vrf_score {
                        debug!(
                            "⏭️ [{}] Rejecting block at height {} with VRF score {} (already have score {})",
                            self.direction, block_height, block.header.vrf_score, existing.header.vrf_score
                        );
                        return Ok(None);
                    }
                    info!(
                        "🎲 [{}] Better VRF score at height {}: {} < {} — switching vote",
                        self.direction,
                        block_height,
                        block.header.vrf_score,
                        existing.header.vrf_score
                    );
                    switching_vote = true;
                }
            }
        }

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // Clear stale votes from previous heights so the "first vote wins"
        // anti-double-voting rule doesn't reject votes for this new height.
        consensus.timevote.advance_vote_height(block_height);

        // Phase 3E.1: Cache the block
        let block_hash = block.hash();
        if let Some(cache) = &context.block_cache {
            cache.insert(block_hash, block.clone());
            debug!("💾 Cached block {} for voting", hex::encode(block_hash));
        }

        // Phase 3E.2: Get our node identity and look up our weight
        let validator_id = context
            .node_masternode_address
            .clone()
            .unwrap_or_else(|| format!("node_{}", self.peer_ip));
        let validator_weight = match context.masternode_registry.get(&validator_id).await {
            Some(info) => info.masternode.tier.sampling_weight().max(1),
            None => 1u64, // Default to 1 if not found
        };

        // If switching to a better VRF proposal, clear old vote first so
        // add_vote's "first vote wins" rule doesn't silently drop the new one.
        if switching_vote {
            consensus.timevote.prepare_votes.remove_voter(&validator_id);
        }

        consensus
            .timevote
            .generate_prepare_vote(block_hash, &validator_id, validator_weight);
        info!(
            "✅ [{}] Generated prepare vote for block {} at height {}",
            self.direction,
            hex::encode(block_hash),
            block.header.height
        );

        // Broadcast prepare vote to all peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let sig_bytes = sign_vote(consensus, &block_hash, &validator_id, b"PREPARE");

            let prepare_vote = NetworkMessage::TimeVotePrepare {
                block_hash,
                voter_id: validator_id.clone(),
                signature: sig_bytes,
            };

            match broadcast_tx.send(prepare_vote) {
                Ok(receivers) => {
                    info!(
                        "📤 [{}] Broadcast prepare vote to {} peers",
                        self.direction, receivers
                    );
                }
                Err(_) => {
                    // Channel closed - no active receivers (peers not ready yet)
                    // This is not critical, just log at debug level
                    debug!(
                        "[{}] No active peers to broadcast prepare vote (channel closed)",
                        self.direction
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle TimeLock Prepare Vote - accumulate and check consensus
    pub(super) async fn handle_timelock_prepare_vote(
        &self,
        block_hash: [u8; 32],
        voter_id: String,
        signature: Vec<u8>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "🗳️  [{}] Received prepare vote for block {} from {}",
            self.direction,
            hex::encode(block_hash),
            voter_id
        );

        // Drop relayed copies of our own vote — we already counted it at generation time.
        // Without this guard, peers relay our vote back and the registry lookup fails
        // (we can't look ourselves up by IP when our registration is in transition),
        // flooding the log with "unknown/unregistered voter" warnings.
        if context
            .node_masternode_address
            .as_deref()
            .is_some_and(|local| local == voter_id)
        {
            debug!(
                "📬 [{}] Dropping relayed self-vote for block {} (already counted at generation)",
                self.direction,
                hex::encode(block_hash)
            );
            return Ok(None);
        }

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // Phase 3E.2: Look up voter weight from masternode registry
        let voter_weight = Self::get_voter_weight(&context.masternode_registry, &voter_id).await;

        // Verify vote signature
        if !self
            .verify_vote_signature(
                &context.masternode_registry,
                &block_hash,
                &voter_id,
                b"PREPARE",
                &signature,
                context,
            )
            .await
            .unwrap_or(false)
        {
            return Ok(None); // Reject invalid signature
        }

        consensus
            .timevote
            .accumulate_prepare_vote(block_hash, voter_id.clone(), voter_weight);

        // Gossip the prepare vote to all peers so it reaches the producer even when
        // the originating free node is not directly connected to the producer.
        // Dedup by (block_hash || voter_id || 'P') so we relay each unique vote exactly
        // once — without this, two peers bounce the same vote back and forth indefinitely.
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let mut relay_key = block_hash.to_vec();
            relay_key.extend_from_slice(voter_id.as_bytes());
            relay_key.push(b'P');
            let already_relayed = if let Some(ref seen) = context.seen_votes {
                seen.check_and_insert(&relay_key).await
            } else {
                false
            };
            if !already_relayed {
                let relay = NetworkMessage::TimeVotePrepare {
                    block_hash,
                    voter_id: voter_id.clone(),
                    signature: signature.clone(),
                };
                let _ = broadcast_tx.send(relay);
            }
        }

        // Check if prepare consensus reached (>50% majority timevote)
        if consensus.timevote.check_prepare_consensus(block_hash) {
            info!(
                "✅ [{}] Prepare consensus reached for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Generate precommit vote with actual weight
            let validator_id = context
                .node_masternode_address
                .clone()
                .unwrap_or_else(|| format!("node_{}", self.peer_ip));
            let validator_weight =
                Self::get_voter_weight(&context.masternode_registry, &validator_id)
                    .await
                    .max(1);

            let self_signature = sign_vote(consensus, &block_hash, &validator_id, b"PRECOMMIT");

            consensus.timevote.generate_precommit_vote(
                block_hash,
                &validator_id,
                validator_weight,
                self_signature.clone(),
            );
            debug!(
                "✅ [{}] Generated precommit vote for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Broadcast precommit vote
            if let Some(broadcast_tx) = &context.broadcast_tx {
                let precommit_vote = NetworkMessage::TimeVotePrecommit {
                    block_hash,
                    voter_id: validator_id,
                    signature: self_signature,
                };

                match broadcast_tx.send(precommit_vote) {
                    Ok(receivers) => {
                        debug!(
                            "📤 [{}] Broadcast precommit vote to {} peers",
                            self.direction, receivers
                        );
                    }
                    Err(_) => {
                        warn!(
                            "[{}] ⚠️  No active peers to broadcast precommit vote (channel closed)",
                            self.direction
                        );
                    }
                }
            }
        }

        Ok(None)
    }

    /// Handle TimeLock Precommit Vote - accumulate and finalize if consensus reached
    pub(super) async fn handle_timelock_precommit_vote(
        &self,
        block_hash: [u8; 32],
        voter_id: String,
        signature: Vec<u8>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "🗳️  [{}] Received precommit vote for block {} from {}",
            self.direction,
            hex::encode(block_hash),
            voter_id
        );

        // Drop relayed copies of our own vote — same reasoning as the PREPARE handler.
        if context
            .node_masternode_address
            .as_deref()
            .is_some_and(|local| local == voter_id)
        {
            debug!(
                "📬 [{}] Dropping relayed self-precommit for block {} (already counted at generation)",
                self.direction,
                hex::encode(block_hash)
            );
            return Ok(None);
        }

        // Get consensus engine or return error
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // Phase 3E.2: Look up voter weight from masternode registry
        let voter_weight = Self::get_voter_weight(&context.masternode_registry, &voter_id).await;

        // Verify vote signature
        if !self
            .verify_vote_signature(
                &context.masternode_registry,
                &block_hash,
                &voter_id,
                b"PRECOMMIT",
                &signature,
                context,
            )
            .await
            .unwrap_or(false)
        {
            return Ok(None); // Reject invalid signature
        }

        consensus.timevote.accumulate_precommit_vote(
            block_hash,
            voter_id.clone(),
            voter_weight,
            signature.clone(),
        );

        // Gossip the precommit vote to all peers for the same reason as prepare votes.
        // Same dedup key pattern: (block_hash || voter_id || 'C').
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let mut relay_key = block_hash.to_vec();
            relay_key.extend_from_slice(voter_id.as_bytes());
            relay_key.push(b'C');
            let already_relayed = if let Some(ref seen) = context.seen_votes {
                seen.check_and_insert(&relay_key).await
            } else {
                false
            };
            if !already_relayed {
                let relay = NetworkMessage::TimeVotePrecommit {
                    block_hash,
                    voter_id: voter_id.clone(),
                    signature: signature.clone(),
                };
                let _ = broadcast_tx.send(relay);
            }
        }

        // Check if precommit consensus reached (>50% majority timevote)
        if consensus.timevote.check_precommit_consensus(block_hash) {
            info!(
                "✅ [{}] Precommit consensus reached for block {}",
                self.direction,
                hex::encode(block_hash)
            );

            // Phase 3E.3: Finalization Callback
            // 1. Retrieve the block from cache
            if let Some(cache) = &context.block_cache {
                if let Some(block) = cache.remove(&block_hash) {
                    // 2. Collect precommit signatures for finality proof
                    let precommit_weight = consensus.timevote.get_precommit_weight(block_hash);
                    let signatures = consensus.timevote.get_precommit_signatures(block_hash);
                    debug!(
                        "📋 [{}] Collected {} precommit signatures for block {} (total weight: {})",
                        self.direction,
                        signatures.len(),
                        hex::encode(block_hash),
                        precommit_weight
                    );

                    // 3. Phase 3E.3: Call timelock.finalize_block_complete()
                    // Note: This would be called through a TimeLock module instance
                    // For now, emit finalization event
                    info!(
                        "🎉 [{}] Block {} finalized with consensus!",
                        self.direction,
                        hex::encode(block_hash)
                    );
                    debug!(
                        "📦 Block height: {}, txs: {}",
                        block.header.height,
                        block.transactions.len()
                    );

                    // 4. Emit finalization event
                    // Calculate reward - constant 100 TIME per block
                    const BLOCK_REWARD_SATOSHIS: u64 = 100 * 100_000_000; // 100 TIME
                    let block_subsidy = BLOCK_REWARD_SATOSHIS;
                    let tx_fees: u64 = block.transactions.iter().map(|tx| tx.fee_amount()).sum();
                    let total_reward = block_subsidy + tx_fees;

                    debug!(
                        "💰 [{}] Block {} rewards - subsidy: {}, fees: {}, total: {:.2} TIME",
                        self.direction,
                        block.header.height,
                        block_subsidy / 100_000_000,
                        tx_fees / 100_000_000,
                        total_reward as f64 / 100_000_000.0
                    );

                    // Add block to blockchain (if not already present)
                    let current_height = context.blockchain.get_height();

                    // Skip adding genesis block if chain already has blocks
                    if block.header.height == 0 && current_height > 0 {
                        debug!(
                            "[{}] Skipping finalization add for genesis block (chain at height {})",
                            self.direction, current_height
                        );
                    } else if block.header.height > current_height {
                        let block_height = block.header.height; // Store height before move
                        info!(
                            "📥 [{}] Adding finalized block {} at height {} to blockchain (current: {})",
                            self.direction,
                            hex::encode(block_hash),
                            block_height,
                            current_height
                        );
                        match context.blockchain.add_block_with_fork_handling(block).await {
                            Ok(true) => {
                                info!(
                                    "✅ [{}] Block {} finalized via consensus!",
                                    self.direction, block_height
                                );
                                // Update all connected peers' chain tips to the new height.
                                // All peers participated in consensus, so they should all
                                // have this block — prevents stale tips causing phantom forks.
                                let connected = context.peer_registry.get_connected_peers().await;
                                for peer in &connected {
                                    context
                                        .peer_registry
                                        .update_peer_chain_tip(peer, block_height, block_hash)
                                        .await;
                                }
                            }
                            Ok(false) => {
                                debug!(
                                    "[{}] Block {} already in blockchain, skipping",
                                    self.direction, block_height
                                );
                            }
                            Err(e) => {
                                if e.contains("Block height mismatch") {
                                    let gap = block_height - current_height;
                                    warn!(
                                        "[{}] ⚠️ Block height gap detected: expected {}, got {} (gap: {})",
                                        self.direction, current_height + 1, block_height, gap
                                    );

                                    // Trigger automatic sync to fill the gap
                                    info!(
                                        "📥 Requesting missing blocks {}-{} from {}",
                                        current_height + 1,
                                        block_height - 1,
                                        self.peer_ip
                                    );

                                    let sync_msg = NetworkMessage::GetBlocks(
                                        current_height + 1,
                                        block_height - 1,
                                    );

                                    if let Err(send_err) = context
                                        .peer_registry
                                        .send_to_peer(&self.peer_ip, sync_msg)
                                        .await
                                    {
                                        warn!("Failed to request missing blocks: {}", send_err);
                                    }
                                } else {
                                    warn!(
                                        "[{}] ⚠️ Failed to add finalized block {} to blockchain: {}",
                                        self.direction, block_height, e
                                    );
                                }
                            }
                        }
                    } else {
                        debug!(
                            "[{}] Block {} already in blockchain at height {}, skipping add",
                            self.direction,
                            hex::encode(block_hash),
                            block.header.height
                        );
                    }
                    // Save precommit voters for bitmap ONLY on first finalization
                    // (cache.remove ensures this runs once — late precommits won't overwrite)
                    consensus.timevote.cleanup_block_votes(block_hash);
                } else {
                    debug!(
                        "[{}] Block {} not found in cache (likely already finalized)",
                        self.direction,
                        hex::encode(block_hash)
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle FinalityVoteBroadcast- verify signature and accumulate vote
    pub(super) async fn handle_finality_vote_broadcast(
        &self,
        vote: crate::types::FinalityVote,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "[{}] Received finality vote for tx {} from {}",
            self.direction,
            hex::encode(vote.txid),
            vote.voter_mn_id
        );

        // Get voter's public key from masternode registry
        let voter_pubkey = match context.masternode_registry.get(&vote.voter_mn_id).await {
            Some(mn_info) => mn_info.masternode.public_key,
            None => {
                warn!(
                    "[{}] Received finality vote from unknown validator: {}",
                    self.direction, vote.voter_mn_id
                );
                return Ok(None);
            }
        };

        // Verify the vote signature
        if let Err(e) = vote.verify(&voter_pubkey) {
            warn!(
                "[{}] Invalid finality vote signature from {}: {}",
                self.direction, vote.voter_mn_id, e
            );
            return Ok(None);
        }

        debug!(
            "[{}] ✅ Verified finality vote signature from {}",
            self.direction, vote.voter_mn_id
        );

        // Accumulate the vote for VFP assembly
        if let Some(consensus) = &context.consensus {
            if let Err(e) = consensus.timevote.accumulate_finality_vote(vote) {
                warn!(
                    "[{}] Failed to accumulate finality vote: {}",
                    self.direction, e
                );
            }
        }

        Ok(None)
    }

    // ==================== NEW HANDLERS ====================

    /// Handle GetBlockHeight request
    pub(super) async fn handle_consensus_query(
        &self,
        height: u64,
        block_hash: [u8; 32],
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received ConsensusQuery for height {} from {}",
            self.direction, height, self.peer_ip
        );

        let (agrees, our_hash) = context
            .blockchain
            .check_consensus_with_peer(height, block_hash)
            .await;
        Ok(Some(NetworkMessage::ConsensusQueryResponse {
            agrees,
            height,
            their_hash: our_hash.unwrap_or([0u8; 32]),
        }))
    }

    /// Handle GetChainWork
    pub(super) async fn handle_get_chain_work(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        let tip_hash = context
            .blockchain
            .get_block_hash_at_height(height)
            .await
            .unwrap_or([0u8; 32]);
        let cumulative_work = context.blockchain.get_cumulative_work().await;

        debug!(
            "📤 [{}] Sending chain work response to {}: height={}, work={}",
            self.direction, self.peer_ip, height, cumulative_work
        );
        Ok(Some(NetworkMessage::ChainWorkResponse {
            height,
            tip_hash,
            cumulative_work,
        }))
    }

    /// Handle GetChainWorkAt
    pub(super) async fn handle_get_chain_work_at(
        &self,
        height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_hash = context
            .blockchain
            .get_block_hash_at_height(height)
            .await
            .unwrap_or([0u8; 32]);
        let cumulative_work = context
            .blockchain
            .get_work_at_height(height)
            .await
            .unwrap_or(0);

        debug!(
            "📤 [{}] Sending chain work at height {} to {}",
            self.direction, height, self.peer_ip
        );
        Ok(Some(NetworkMessage::ChainWorkAtResponse {
            height,
            block_hash,
            cumulative_work,
        }))
    }

    /// Handle ForkAlert
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_fork_alert(
        &self,
        your_height: u64,
        your_hash: [u8; 32],
        consensus_height: u64,
        consensus_hash: [u8; 32],
        consensus_peer_count: usize,
        message: String,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let is_syncing = context.blockchain.is_syncing();
        let our_height = context.blockchain.get_height();

        // Suppress fork alerts when the node is clearly still catching up.
        // `is_syncing` is only true during sync_from_peers(); inbound-pushed blocks
        // leave it false even while hundreds of blocks behind. Use a height-gap check
        // as the broader gate so we don't WARN during normal catch-up.
        let significantly_behind = consensus_height > our_height + 10;

        if is_syncing || significantly_behind {
            // We already know we're behind and are actively catching up.
            // Update peer chain tip so sync_from_peers can use this peer,
            // but suppress all warnings and redundant GetBlocks requests.
            debug!(
                "🚨 [{}] FORK ALERT from {} (suppressed — catching up, our height {} vs consensus {}): {}",
                self.direction, self.peer_ip, our_height, consensus_height, message
            );
            context
                .peer_registry
                .update_peer_chain_tip(&self.peer_ip, consensus_height, consensus_hash)
                .await;
            context
                .peer_registry
                .set_peer_height(&self.peer_ip, consensus_height)
                .await;
            return Ok(None);
        }

        warn!(
            "🚨 [{}] FORK ALERT from {}: {}",
            self.direction, self.peer_ip, message
        );
        warn!(
            "   Our height {} hash {} vs Consensus height {} hash {} ({} peers)",
            your_height,
            hex::encode(&your_hash[..8]),
            consensus_height,
            hex::encode(&consensus_hash[..8]),
            consensus_peer_count
        );

        // If we're on the minority fork, request consensus chain.
        // Check both same-height forks and height mismatches (we may have
        // advanced further on our fork, or fallen behind).
        let our_hash_differs = your_hash != consensus_hash;
        let we_are_behind = consensus_height > your_height;

        if our_hash_differs || we_are_behind {
            // ── AV30: Fork Alert Spam guard ─────────────────────────────────
            // A legitimate peer sends a fork alert once when it detects a fork.
            // A fork-bombing attacker sends them every few seconds, causing us
            // to repeatedly download and discard their invalid chain — a CPU/
            // bandwidth DoS that can crash the node via task-spawn accumulation.
            //
            // Strategy: maintain a per-peer (last_getblocks_sent, rejected_cycles,
            // window_start) triple.  If we sent GetBlocks to this peer within
            // FORK_ALERT_RESPONSE_COOLDOWN and their last response was rejected,
            // suppress this alert.  After FORK_ALERT_BAN_THRESHOLD rejected cycles
            // within FORK_ALERT_WINDOW, record a banlist violation (→ eventual ban).
            {
                let now = Instant::now();
                let tracker = incoming_fork_alert_tracker();
                let mut entry = tracker
                    .entry(self.peer_ip.clone())
                    .or_insert_with(|| (now, 0u32, now));
                let (last_sent, rejected_cycles, window_start) = *entry;

                // Reset window if it has expired
                let in_window = now.duration_since(window_start) < FORK_ALERT_WINDOW;
                let cycles = if in_window { rejected_cycles } else { 0 };

                if cycles >= FORK_ALERT_BAN_THRESHOLD {
                    // Peer has been persistently bombing us — record violation
                    warn!(
                        "🚫 [AV30] Fork alert spam from {} — {} rejected cycles in {}s, recording violation",
                        self.peer_ip,
                        cycles,
                        FORK_ALERT_WINDOW.as_secs()
                    );
                    if let Some(bl) = &context.banlist {
                        let bare = self.peer_ip.split(':').next().unwrap_or(&self.peer_ip);
                        if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
                            bl.write().await.record_violation(
                                ip,
                                &format!("AV30 fork alert spam: {} rejected cycles", cycles),
                            );
                        }
                    }
                    // Reset counter so ban escalation works correctly
                    *entry = (last_sent, 0, now);
                    return Ok(None);
                }

                // If we already sent GetBlocks recently and it led to a rejection, suppress
                if in_window
                    && cycles > 0
                    && now.duration_since(last_sent) < FORK_ALERT_RESPONSE_COOLDOWN
                {
                    debug!(
                        "⏸️ [AV30] Suppressing duplicate fork alert response to {} ({} rejected cycles)",
                        self.peer_ip, cycles
                    );
                    return Ok(None);
                }

                // Also suppress if we sent GetBlocks very recently (regardless of rejection)
                if now.duration_since(last_sent) < FORK_ALERT_RESPONSE_COOLDOWN {
                    debug!(
                        "⏸️ [AV30] Fork alert cooldown active for {} (sent GetBlocks {}s ago)",
                        self.peer_ip,
                        now.duration_since(last_sent).as_secs()
                    );
                    return Ok(None);
                }

                // Record that we are now sending GetBlocks
                *entry = (now, cycles, if in_window { window_start } else { now });
            }
            // ── End AV30 guard ──────────────────────────────────────────────

            warn!(
                "   ⚠️ We appear to be on minority fork (our height {} vs consensus {})! Requesting consensus chain...",
                your_height, consensus_height
            );

            // Update the alerting peer's chain tip so sync_from_peers can find them.
            // Without this, the chain tip cache stays stale and sync_from_peers
            // concludes "no peers have blocks beyond our height".
            context
                .peer_registry
                .update_peer_chain_tip(&self.peer_ip, consensus_height, consensus_hash)
                .await;
            context
                .peer_registry
                .set_peer_height(&self.peer_ip, consensus_height)
                .await;

            // Start request before our tip for chain validation overlap
            let request_from = if we_are_behind {
                your_height.saturating_sub(5)
            } else {
                consensus_height.saturating_sub(10)
            };
            let request_to = your_height.max(consensus_height) + 5;
            return Ok(Some(NetworkMessage::GetBlocks(request_from, request_to)));
        }

        Ok(None)
    }

    /// Handle ForkAlert
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_liveness_alert(
        &self,
        alert: crate::types::LivenessAlert,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let txid_hex = hex::encode(alert.txid);

        info!(
            "[{}] Received LivenessAlert for tx {} from {} (stall: {}ms, confidence: {})",
            self.direction,
            txid_hex,
            alert.reporter_mn_id,
            alert.stall_duration_ms,
            alert.current_confidence
        );

        // Verify the alert signature - find masternode by address
        let masternodes = context.masternode_registry.list_all().await;
        let masternode = masternodes
            .iter()
            .find(|mn| mn.masternode.address == alert.reporter_mn_id)
            .ok_or_else(|| {
                format!(
                    "Reporter {} not in masternode registry",
                    alert.reporter_mn_id
                )
            })?;

        alert
            .verify(&masternode.masternode.public_key)
            .map_err(|e| format!("Invalid LivenessAlert signature: {}", e))?;

        // Forward to consensus engine if we have one
        if let Some(consensus) = &context.consensus {
            // Phase 4: Detect equivocation before processing
            if consensus.detect_alert_equivocation(&alert.txid, &alert.reporter_mn_id) {
                consensus.flag_byzantine(&alert.reporter_mn_id, "Alert equivocation detected");
                return Err(format!(
                    "Rejecting alert from {}: equivocation detected",
                    alert.reporter_mn_id
                ));
            }

            // Check if we also observe this stall
            if let Some(tx_status) = consensus.get_tx_status(&alert.txid) {
                if matches!(tx_status, crate::types::TransactionStatus::Voting { .. }) {
                    // We also see this transaction in Voting state
                    let stalled = consensus.check_stall_timeout(&alert.txid);

                    if stalled {
                        info!("[{}] Confirming stall for tx {}", self.direction, txid_hex);

                        // §7.6 Week 5-6: Accumulate alerts and check f+1 threshold
                        let should_trigger_fallback =
                            consensus.accumulate_liveness_alert(alert.clone(), masternodes.len());

                        let alert_count = consensus.get_alert_count(&alert.txid);
                        let n = masternodes.len();
                        let f = (n.saturating_sub(1)) / 3;
                        let threshold = f + 1;

                        info!(
                            "[{}] Alert accumulation for tx {}: {}/{} (threshold: {})",
                            self.direction, txid_hex, alert_count, n, threshold
                        );

                        // Trigger fallback if f+1 threshold reached
                        if should_trigger_fallback {
                            warn!(
                                "[{}] 🚨 Fallback triggered for tx {} ({} >= {} alerts)",
                                self.direction, txid_hex, alert_count, threshold
                            );

                            // Transition to FallbackResolution state
                            consensus
                                .transition_to_fallback_resolution(alert.txid, alert_count as u32);
                        }
                    }
                }
            }
        }

        // Relay the alert to other peers (gossip protocol)
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let _ = broadcast_tx.send(NetworkMessage::LivenessAlert { alert });
        }

        Ok(None)
    }

    /// Handle FinalityProposal message (§7.6.4 Step 3)
    /// Deterministic leader proposes Accept/Reject decision
    pub(super) async fn handle_finality_proposal(
        &self,
        proposal: crate::types::FinalityProposal,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let txid_hex = hex::encode(proposal.txid);

        info!(
            "[{}] Received FinalityProposal for tx {} from leader {} (decision: {:?})",
            self.direction, txid_hex, proposal.leader_mn_id, proposal.decision
        );

        // Verify the proposal signature - find masternode by address
        let masternodes = context.masternode_registry.list_all().await;
        let leader = masternodes
            .iter()
            .find(|mn| mn.masternode.address == proposal.leader_mn_id)
            .ok_or_else(|| {
                format!(
                    "Leader {} not in masternode registry",
                    proposal.leader_mn_id
                )
            })?;

        proposal
            .verify(&leader.masternode.public_key)
            .map_err(|e| format!("Invalid FinalityProposal signature: {}", e))?;

        // §7.6 Week 5-6 Part 2: Register proposal and prepare for voting
        if let Some(consensus) = &context.consensus {
            // Register the mapping so we can finalize when votes come in
            let proposal_hash = proposal.proposal_hash();
            consensus.register_proposal(proposal_hash, proposal.txid);

            info!(
                "[{}] Registered proposal {} for tx {}",
                self.direction,
                hex::encode(proposal_hash),
                txid_hex
            );

            // Phase 4: Detect Byzantine behavior (multiple proposals for same tx)
            let proposals_for_tx = consensus.detect_multiple_proposals(&proposal.txid);
            if proposals_for_tx.len() > 1 {
                consensus.flag_byzantine(
                    &proposal.leader_mn_id,
                    "Multiple proposals for same transaction",
                );
                warn!(
                    "[{}] ⚠️ Multiple proposals detected for tx {} by leader {}",
                    self.direction, txid_hex, proposal.leader_mn_id
                );
            }

            // §7.6 Week 5-6 Part 3: Verify leader and cast vote
            // Step 1: Compute who the expected leader should be
            let avs = masternodes
                .iter()
                .filter(|mn| mn.is_active)
                .map(|mn| mn.masternode.clone())
                .collect::<Vec<_>>();

            let prev_block_hash = consensus.get_prev_block_hash();
            let expected_leader = crate::consensus::compute_fallback_leader(
                &proposal.txid,
                proposal.slot_index,
                &avs,
                &prev_block_hash,
            );

            // Step 2: Verify the proposal came from the expected leader
            match expected_leader {
                Some(expected_mn_id) if expected_mn_id == proposal.leader_mn_id => {
                    info!(
                        "[{}] ✅ Leader verified: {} is correct leader for slot {}",
                        self.direction, proposal.leader_mn_id, proposal.slot_index
                    );

                    // Step 3: Decide how to vote based on transaction state
                    let vote_decision = consensus.decide_fallback_vote(&proposal.txid);

                    info!(
                        "[{}] Voting {:?} on proposal {} (tx {})",
                        self.direction,
                        vote_decision,
                        hex::encode(proposal_hash),
                        txid_hex
                    );

                    // Step 4: Get our voting weight from the masternode registry and broadcast vote
                    let our_id = context
                        .node_masternode_address
                        .as_deref()
                        .unwrap_or_default();
                    let voter_weight =
                        Self::get_voter_weight(&context.masternode_registry, our_id).await;

                    if let Err(e) = consensus
                        .broadcast_fallback_vote(proposal_hash, vote_decision, voter_weight)
                        .await
                    {
                        warn!("[{}] Failed to broadcast vote: {}", self.direction, e);
                    }
                }
                Some(expected_mn_id) => {
                    warn!(
                        "[{}] ❌ Invalid leader: expected {}, got {} (ignoring proposal)",
                        self.direction, expected_mn_id, proposal.leader_mn_id
                    );
                    // Don't vote on invalid leader proposals
                    return Ok(None);
                }
                None => {
                    warn!(
                        "[{}] ⚠️ Could not compute expected leader (empty AVS?)",
                        self.direction
                    );
                    return Ok(None);
                }
            }
        }

        // Relay the proposal to other peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let _ = broadcast_tx.send(NetworkMessage::FinalityProposal { proposal });
        }

        Ok(None)
    }

    /// Handle FallbackVote message (§7.6.4 Step 4)
    /// AVS member votes on leader's proposal
    pub(super) async fn handle_fallback_vote(
        &self,
        vote: crate::types::FallbackVote,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let proposal_hex = hex::encode(vote.proposal_hash);

        debug!(
            "[{}] Received FallbackVote for proposal {} from {} (vote: {:?}, weight: {})",
            self.direction, proposal_hex, vote.voter_mn_id, vote.vote, vote.voter_weight
        );

        // Verify the vote signature - find masternode by address.
        // Use list_active() so disconnected nodes (removed or marked inactive) cannot
        // cast votes or contribute to total_avs_weight.
        let masternodes = context.masternode_registry.list_active().await;
        let voter = masternodes
            .iter()
            .find(|mn| mn.masternode.address == vote.voter_mn_id)
            .ok_or_else(|| format!("Voter {} not in masternode registry", vote.voter_mn_id))?;

        vote.verify(&voter.masternode.public_key)
            .map_err(|e| format!("Invalid FallbackVote signature: {}", e))?;

        // §7.6 Week 5-6 Part 2: Accumulate votes and check Q_finality threshold
        if let Some(consensus) = &context.consensus {
            // Phase 4: Detect vote equivocation before processing
            if consensus.detect_vote_equivocation(&vote.proposal_hash, &vote.voter_mn_id) {
                consensus.flag_byzantine(&vote.voter_mn_id, "Vote equivocation detected");
                return Err(format!(
                    "Rejecting vote from {}: equivocation detected",
                    vote.voter_mn_id
                ));
            }

            // Calculate total AVS weight (sum of all masternode sampling weights)
            let total_avs_weight: u64 = masternodes
                .iter()
                .map(|mn| mn.masternode.tier.sampling_weight().max(1))
                .sum();

            // Phase 4: Validate vote weight doesn't exceed total
            if let Err(e) = consensus.validate_vote_weight(&vote.proposal_hash, total_avs_weight) {
                warn!(
                    "[{}] ⚠️ Invalid vote weight for proposal {}: {}",
                    self.direction, proposal_hex, e
                );
            }

            // Accumulate vote and check if quorum reached
            if let Some(decision) =
                consensus.accumulate_fallback_vote(vote.clone(), total_avs_weight)
            {
                // Q_finality threshold reached! Finalize the transaction

                info!(
                    "[{}] 🎯 Q_finality reached for proposal {} (decision: {:?})",
                    self.direction, proposal_hex, decision
                );

                // Get the transaction ID for this proposal
                if let Some(txid) = consensus.get_proposal_txid(&vote.proposal_hash) {
                    let txid_hex = hex::encode(txid);

                    // Calculate total weight that voted for winning decision
                    let (approve_weight, reject_weight, vote_count) = consensus
                        .get_vote_status(&vote.proposal_hash)
                        .unwrap_or((0, 0, 0));

                    let winning_weight = match decision {
                        crate::types::FallbackVoteDecision::Approve => approve_weight,
                        crate::types::FallbackVoteDecision::Reject => reject_weight,
                    };

                    info!(
                        "[{}] Finalizing tx {} via fallback: {:?} (weight: {}/{}, votes: {})",
                        self.direction,
                        txid_hex,
                        decision,
                        winning_weight,
                        total_avs_weight,
                        vote_count
                    );

                    // Finalize the transaction
                    consensus.finalize_from_fallback(txid, decision, winning_weight);
                } else {
                    warn!(
                        "[{}] ⚠️  Quorum reached but no txid mapping for proposal {}",
                        self.direction, proposal_hex
                    );
                }
            } else {
                // Calculate current vote status for logging
                if let Some((approve_weight, reject_weight, vote_count)) =
                    consensus.get_vote_status(&vote.proposal_hash)
                {
                    let q_finality = (total_avs_weight * 2) / 3;
                    debug!(
                        "[{}] Vote accumulated for proposal {}: Approve={}, Reject={}, Total votes={}, Q_finality={}",
                        self.direction,
                        proposal_hex,
                        approve_weight,
                        reject_weight,
                        vote_count,
                        q_finality
                    );
                }
            }
        }

        // Relay the vote to other peers
        if let Some(broadcast_tx) = &context.broadcast_tx {
            let _ = broadcast_tx.send(NetworkMessage::FallbackVote { vote });
        }

        Ok(None)
    }

    /// CRITICAL SECURITY: Validate block before voting to prevent consensus on invalid blocks
    ///
    /// This validation must happen BEFORE voting to ensure:
    /// - Invalid blocks don't accumulate votes
    /// - Network doesn't waste resources on invalid proposals
    /// - Consensus can't finalize blocks that will be rejected during add_block()
    async fn validate_block_before_vote(
        &self,
        block: &Block,
        context: &MessageContext,
    ) -> Result<(), String> {
        // 1. Validate block structure and size
        let serialized =
            bincode::serialize(block).map_err(|e| format!("Failed to serialize block: {}", e))?;

        const MAX_BLOCK_SIZE: usize = 4 * 1024 * 1024; // 4MB
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!(
                "Block too large: {} bytes (max {})",
                serialized.len(),
                MAX_BLOCK_SIZE
            ));
        }

        // 2. Validate merkle root
        let computed_merkle = calculate_merkle_root(&block.transactions);
        if block.header.merkle_root != computed_merkle {
            return Err(format!(
                "Invalid merkle root: expected {}, got {}",
                hex::encode(computed_merkle),
                hex::encode(block.header.merkle_root)
            ));
        }

        // 3. Validate block must have at least 2 transactions (coinbase + reward_distribution)
        if block.transactions.len() < 2 {
            return Err(format!(
                "Block has {} transactions, expected at least 2",
                block.transactions.len()
            ));
        }

        // 4. Validate block rewards (prevents double-counting and inflation)
        // Skip for genesis block
        if block.header.height > 0 {
            self.validate_block_rewards_structure(block)?;
        }

        // 5. SECURITY: Verify VRF proof — confirms proposer is legitimately selected.
        // MUST run before reward validation (step 4b) so that forged proposals with a
        // victim's IP as `leader` cannot poison the victim's reward-violation counter
        // (AV36 — reputation poisoning via unauthenticated block proposals).
        // Skip for old blocks without VRF proof (backward compatibility).
        //
        // `leader_authenticated`: true iff VRF proof is present AND verifies correctly.
        // Reward violations are only recorded against the leader when this is true;
        // otherwise the violation is recorded against the sending peer instead.
        let leader_authenticated = if !block.header.vrf_proof.is_empty() && block.header.height > 0
        {
            // Look up the proposer's public key from masternode registry
            let proposer = block.header.leader.clone();
            if proposer.is_empty() {
                return Err("Block has VRF proof but no leader set".to_string());
            }

            let proposer_info = context
                .masternode_registry
                .get(&proposer)
                .await
                .ok_or_else(|| {
                    format!(
                        "Block proposer {} not found in masternode registry",
                        proposer
                    )
                })?;

            // Anti-sybil: reject blocks from immature Free-tier proposers
            if !crate::masternode_registry::MasternodeRegistry::is_mature_for_sortition(
                &proposer_info,
                block.header.height,
                context.masternode_registry.network(),
            ) {
                return Err(format!(
                    "Block proposer {} is an immature Free-tier node (registered at height {}, current {})",
                    proposer, proposer_info.registration_height, block.header.height
                ));
            }

            // Verify the VRF proof using the proposer's public key
            crate::block::vrf::verify_block_vrf(
                &proposer_info.masternode.public_key,
                block.header.height,
                &block.header.previous_hash,
                &block.header.vrf_proof,
                &block.header.vrf_output,
            )?;

            // Verify vrf_score matches vrf_output
            let expected_score = crate::block::vrf::vrf_output_to_score(&block.header.vrf_output);
            if block.header.vrf_score != expected_score {
                return Err(format!(
                    "VRF score mismatch: header={}, computed={}",
                    block.header.vrf_score, expected_score
                ));
            }

            // Verify the proposer's VRF score qualifies them (sampling weight + fairness bonus).
            // Use the in-memory counter (same source as the producer) so both sides compute
            // identical bonuses and thresholds. The blockchain-scan alternative
            // (get_verifiable_reward_tracking) resets only on leader blocks while pool rewards
            // reset the in-memory counter every block for all nodes, causing drift.
            let blocks_without_reward_map = context
                .masternode_registry
                .get_reward_tracking_from_memory()
                .await;

            let proposer_blocks_without = blocks_without_reward_map
                .get(&proposer)
                .copied()
                .unwrap_or(0);
            let proposer_fairness_bonus = proposer_blocks_without / 10;
            // Apply the same Free-tier cap as the producer's self-selection code so that
            // both sides compute identical thresholds (prevents spurious validator rejections).
            let proposer_weight = {
                let raw = proposer_info.masternode.tier.sampling_weight() + proposer_fairness_bonus;
                if matches!(
                    proposer_info.masternode.tier,
                    crate::types::MasternodeTier::Free
                ) {
                    raw.min(crate::types::MasternodeTier::Bronze.sampling_weight() - 1)
                } else {
                    raw
                }
            };

            let eligible_masternodes = context
                .masternode_registry
                .get_vrf_eligible(block.header.height)
                .await;
            let total_sampling_weight: u64 = eligible_masternodes
                .iter()
                .map(|(mn, _)| {
                    let bonus = blocks_without_reward_map
                        .get(&mn.address)
                        .copied()
                        .map(|b| b / 10)
                        .unwrap_or(0);
                    let raw = mn.tier.sampling_weight() + bonus;
                    if matches!(mn.tier, crate::types::MasternodeTier::Free) {
                        raw.min(crate::types::MasternodeTier::Bronze.sampling_weight() - 1)
                    } else {
                        raw
                    }
                })
                .sum();

            if total_sampling_weight > 0 {
                let is_eligible = crate::block::vrf::vrf_check_proposer_eligible(
                    block.header.vrf_score,
                    proposer_weight,
                    total_sampling_weight,
                );

                if !is_eligible {
                    // Allow relaxed threshold during timeout — same exponential backoff as
                    // the producer (main.rs: 1u64 << leader_attempt.min(20), one attempt
                    // per LEADER_TIMEOUT_SECS=30s). Cap timeout_attempts at 20 to match.
                    let our_height = context.blockchain.get_height();
                    let expected_height = our_height + 1;
                    if block.header.height == expected_height {
                        let genesis_ts = context.blockchain.genesis_timestamp();
                        let slot_time = genesis_ts + (block.header.height as i64 * 600);
                        let now = chrono::Utc::now().timestamp();
                        let elapsed = (now - slot_time).max(0) as u64;
                        // Cap at 20 to match producer's `leader_attempt.min(20)` cap.
                        let timeout_attempts = (elapsed / 30).min(20);

                        if timeout_attempts > 0 {
                            let multiplier = 1u64 << timeout_attempts;
                            let relaxed_weight = proposer_weight
                                .saturating_mul(multiplier)
                                .min(total_sampling_weight);
                            let eligible_relaxed = crate::block::vrf::vrf_check_proposer_eligible(
                                block.header.vrf_score,
                                relaxed_weight,
                                total_sampling_weight,
                            );
                            if !eligible_relaxed {
                                return Err(format!(
                                    "Proposer {} VRF score {} exceeds threshold (even with {}x relaxation after {}s)",
                                    proposer, block.header.vrf_score, multiplier, elapsed
                                ));
                            }
                            debug!(
                                "🎲 [{}] Block {} proposer {} accepted with relaxed VRF threshold ({}x after {}s)",
                                self.direction, block.header.height, proposer, multiplier, elapsed
                            );
                        } else {
                            return Err(format!(
                                "Proposer {} VRF score {} exceeds threshold (weight {}/{}, slot just started)",
                                proposer,
                                block.header.vrf_score,
                                proposer_weight,
                                total_sampling_weight
                            ));
                        }
                    }
                    // height != expected_height: block is for a non-tip height (sync),
                    // VRF eligibility was already checked when it was first proposed.
                }
            }

            debug!(
                "🎲 [{}] Block {} VRF verified: proposer={}, score={}",
                self.direction, block.header.height, proposer, block.header.vrf_score
            );

            // 6. Verify producer signature over block hash.
            // Warn only (not rejection) to match add_block() behaviour — stale registry
            // keys during bootstrap can cause false rejections of valid blocks.
            if let Err(e) = block.verify_signature(&proposer_info.masternode.public_key) {
                tracing::warn!(
                    "Block {} producer signature mismatch (stale key?): {}",
                    block.header.height,
                    e
                );
            }

            true // VRF proof present and verified — leader is authenticated
        } else {
            false // No VRF proof — cannot authenticate leader identity
        };

        // 4b. Validate reward distribution and check producer misbehavior.
        // `leader_authenticated` controls whether failures record violations against the
        // leader address (true) or are silently rejected (false).  When false the caller
        // records a violation against self.peer_ip — the actual sender — instead, so an
        // attacker cannot poison a victim node's reputation by forging its IP as leader.
        if block.header.height > 0 {
            if let Err(e) = context
                .blockchain
                .validate_proposal_rewards(block, leader_authenticated)
                .await
            {
                if !leader_authenticated {
                    // Unauthenticated bad proposal — penalise the *sender*, not the claimed leader
                    warn!(
                        "❌ [{}] Unauthenticated block proposal from {} has bad rewards (AV36): {}",
                        self.direction, self.peer_ip, e
                    );
                    self.record_vote_violation(
                        context,
                        "unauthenticated block proposal with bad rewards (AV36)",
                    )
                    .await;
                }
                return Err(e);
            }
        }

        // 7. Get consensus engine for transaction validation
        let consensus = context
            .consensus
            .as_ref()
            .ok_or_else(|| "Consensus engine not available".to_string())?;

        // 8. Validate all transactions (except coinbase and reward distribution)
        // Transactions 0-1 are system transactions (coinbase + reward_distribution)
        // Transactions 2+ are user transactions that need full validation
        for (idx, tx) in block.transactions.iter().enumerate() {
            if idx < 2 {
                continue; // Skip coinbase and reward distribution (validated separately)
            }

            // TXs already in our finalized pool were validated by TimeVote consensus
            // (67% stake threshold — stricter than the block vote threshold). Their
            // input UTXOs are tombstoned (removed from sled) so validate_transaction
            // would fail with "UTXO not found". Skip re-validation: we already agreed.
            if consensus.tx_pool.is_finalized(&tx.txid()) {
                continue;
            }

            // Validate transaction structure and signatures
            if let Err(e) = consensus.validate_transaction(tx).await {
                return Err(format!("Invalid transaction at index {}: {}", idx, e));
            }
        }

        // 9. Check for double-spends within the block
        let mut spent_in_block = std::collections::HashSet::new();
        for (idx, tx) in block.transactions.iter().enumerate() {
            for input in &tx.inputs {
                let outpoint_key = format!(
                    "{}:{}",
                    hex::encode(input.previous_output.txid),
                    input.previous_output.vout
                );
                if spent_in_block.contains(&outpoint_key) {
                    return Err(format!(
                        "Double-spend detected in block: UTXO {} spent multiple times",
                        outpoint_key
                    ));
                }
                spent_in_block.insert(outpoint_key);
            }

            // Also check if attempting to spend outputs created in same block
            // This is allowed (chained transactions) but needs careful tracking
            debug!(
                "Transaction {} spends {} inputs, creates {} outputs",
                idx,
                tx.inputs.len(),
                tx.outputs.len()
            );
        }

        info!(
            "✅ [{}] Block {} validation passed: {} transactions, {} bytes",
            self.direction,
            block.header.height,
            block.transactions.len(),
            serialized.len()
        );

        Ok(())
    }

    /// Validate block reward structure (similar to blockchain.rs validation)
    pub(super) async fn handle_timevote_request(
        &self,
        txid: [u8; 32],
        tx_hash_commitment: [u8; 32],
        slot_index: u64,
        tx_from_request: Option<crate::types::Transaction>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let consensus = context
            .consensus
            .as_ref()
            .ok_or("No consensus engine available for TimeVoteRequest")?;

        tracing::info!(
            "🗳️  TimeVoteRequest from {} for TX {} (slot {}){}",
            self.peer_ip,
            hex::encode(txid),
            slot_index,
            if tx_from_request.is_some() {
                " [TX included]"
            } else {
                ""
            }
        );

        // Step 1: Get TX from mempool or from request
        let mut tx_opt = consensus.tx_pool.get_pending(&txid);

        if tx_opt.is_none() {
            if let Some(tx_from_req) = tx_from_request {
                // Validate + lock before pool insert so we never store fee=0 ghost
                // entries that can never finalize (stuck pending forever).
                match consensus.lock_and_validate_transaction(&tx_from_req).await {
                    Ok(fee) => {
                        if consensus
                            .tx_pool
                            .add_pending(tx_from_req.clone(), fee)
                            .is_ok()
                        {
                            tx_opt = Some(tx_from_req);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "⚠️ TimeVoteRequest TX {} rejected at insert: {}",
                            hex::encode(txid),
                            e
                        );
                    }
                }
            }
        }

        let decision = if let Some(tx) = tx_opt {
            let actual_commitment = crate::types::TimeVote::calculate_tx_commitment(&tx);
            if actual_commitment != tx_hash_commitment {
                tracing::warn!("⚠️  TX {} commitment mismatch", hex::encode(txid));
                crate::types::VoteDecision::Reject
            } else {
                match consensus.validate_transaction(&tx).await {
                    Ok(_) => {
                        tracing::info!(
                            "✅ TX {} validated successfully for vote",
                            hex::encode(txid)
                        );
                        crate::types::VoteDecision::Accept
                    }
                    Err(e) => {
                        tracing::warn!("⚠️  TX {} validation failed: {}", hex::encode(txid), e);
                        crate::types::VoteDecision::Reject
                    }
                }
            }
        } else {
            // TX not in pending pool and not included in request — check if already
            // finalized (race condition: TX moved from pending to finalized between
            // the vote request being sent and processed). Vote Accept for finalized TXs.
            if consensus.tx_pool.is_finalized(&txid)
                || consensus.timevote.finalized_txs.contains_key(&txid)
            {
                tracing::debug!(
                    "✅ TX {} already finalized, voting Accept for late vote request",
                    hex::encode(txid)
                );
                crate::types::VoteDecision::Accept
            } else {
                tracing::debug!(
                    "⚠️  TX {} not found in mempool and not included in request",
                    hex::encode(txid)
                );
                crate::types::VoteDecision::Reject
            }
        };

        // Sign TimeVote with our masternode key
        let vote_opt = consensus.sign_timevote(txid, tx_hash_commitment, slot_index, decision);

        if let Some(vote) = vote_opt {
            tracing::info!(
                "✅ TimeVoteResponse ready for TX {} (decision: {:?})",
                hex::encode(txid),
                decision
            );
            Ok(Some(NetworkMessage::TimeVoteResponse { vote }))
        } else {
            tracing::info!(
                "ℹ️ TimeVote signing skipped for TX {} (this node is not a registered masternode)",
                hex::encode(txid)
            );
            Ok(None)
        }
    }

    pub(super) async fn handle_timevote_response(
        &self,
        vote: crate::types::TimeVote,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let consensus = context
            .consensus
            .as_ref()
            .ok_or("No consensus engine available for TimeVoteResponse")?;

        let txid = vote.txid;

        tracing::info!(
            "📥 TimeVoteResponse from {} for TX {} (decision: {:?}, weight: {})",
            self.peer_ip,
            hex::encode(txid),
            vote.decision,
            vote.voter_weight
        );

        // Step 1: Accumulate the vote
        let accumulated_weight = match consensus.timevote.accumulate_timevote(vote) {
            Ok(weight) => weight,
            Err(e) => {
                tracing::warn!(
                    "Failed to accumulate vote for TX {}: {}",
                    hex::encode(txid),
                    e
                );
                return Ok(None);
            }
        };

        tracing::info!(
            "Vote accumulated for TX {}, total weight: {}",
            hex::encode(txid),
            accumulated_weight
        );

        // Step 2: Check if finality threshold reached (67% BFT-safe majority)
        let validators = consensus.timevote.get_validators();
        let total_avs_weight: u64 = validators.iter().map(|v| v.weight).sum();
        let finality_threshold = ((total_avs_weight as f64) * 0.67).ceil() as u64;

        tracing::info!(
            "Finality check for TX {}: accumulated={}, threshold={} (67% of {})",
            hex::encode(txid),
            accumulated_weight,
            finality_threshold,
            total_avs_weight
        );

        // Step 3: If threshold met, finalize transaction
        if accumulated_weight >= finality_threshold {
            tracing::info!(
                "🎉 TX {} reached finality threshold! ({} >= {})",
                hex::encode(txid),
                accumulated_weight,
                finality_threshold
            );

            use dashmap::mapref::entry::Entry;
            match consensus.timevote.finalized_txs.entry(txid) {
                Entry::Vacant(e) => {
                    e.insert((
                        crate::consensus::Preference::Accept,
                        std::time::Instant::now(),
                    ));

                    let tx_data = consensus.tx_pool.get_pending(&txid);
                    if consensus.tx_pool.finalize_transaction(txid) {
                        tracing::info!("✅ TX {} moved to finalized pool", hex::encode(txid));

                        // Transition input UTXOs and create output UTXOs
                        if let Some(ref tx) = tx_data {
                            for input in &tx.inputs {
                                consensus
                                    .utxo_manager
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
                                if let Err(e) = consensus.utxo_manager.add_utxo(utxo).await {
                                    tracing::warn!("Failed to add output UTXO vout={}: {}", idx, e);
                                }
                                consensus
                                    .utxo_manager
                                    .update_state(&outpoint, crate::types::UTXOState::Unspent);
                            }
                        }

                        consensus
                            .timevote
                            .record_finalization(txid, accumulated_weight);

                        // Signal WS subscribers about finalized transaction
                        consensus.signal_tx_finalized(txid);

                        match consensus.timevote.assemble_timeproof(txid) {
                            Ok(timeproof) => {
                                tracing::info!(
                                    "📜 TimeProof assembled for TX {} with {} votes",
                                    hex::encode(txid),
                                    timeproof.votes.len()
                                );

                                if let Err(e) = consensus
                                    .finality_proof_mgr
                                    .store_timeproof(timeproof.clone())
                                {
                                    tracing::error!(
                                        "❌ Failed to store TimeProof for TX {}: {}",
                                        hex::encode(txid),
                                        e
                                    );
                                }

                                consensus.broadcast_timeproof(timeproof).await;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "❌ Failed to assemble TimeProof for TX {}: {}",
                                    hex::encode(txid),
                                    e
                                );
                            }
                        }
                    } else {
                        tracing::warn!(
                            "⚠️  Failed to finalize TX {} - not found in pending pool",
                            hex::encode(txid)
                        );
                    }
                }
                Entry::Occupied(_) => {
                    tracing::debug!("TX {} already finalized by another task", hex::encode(txid));
                }
            }
        }

        Ok(None)
    }

    pub(super) async fn handle_timeproof_broadcast(
        &self,
        proof: crate::types::TimeProof,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let consensus = context
            .consensus
            .as_ref()
            .ok_or("No consensus engine available for TimeProofBroadcast")?;

        tracing::info!(
            "📜 Received TimeProof from {} for TX {} with {} votes",
            self.peer_ip,
            hex::encode(proof.txid),
            proof.votes.len()
        );

        match consensus.timevote.verify_timeproof(&proof) {
            Ok(_) => {
                tracing::info!("✅ TimeProof verified for TX {}", hex::encode(proof.txid));

                if let Err(e) = consensus.finality_proof_mgr.store_timeproof(proof) {
                    tracing::error!("❌ Failed to store TimeProof: {}", e);
                } else {
                    tracing::info!("💾 TimeProof stored successfully");
                }
            }
            Err(e) => {
                tracing::warn!("⚠️  Invalid TimeProof from {}: {}", self.peer_ip, e);
            }
        }

        Ok(None)
    }
}
