use super::common::*;
use super::context::MessageContext;
use super::core::GetBlocksRequest;
use super::MessageHandler;
use crate::block::types::Block;
use crate::network::message::NetworkMessage;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

impl MessageHandler {
    pub(super) async fn handle_get_blocks(
        &self,
        start: u64,
        end: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_height = context.blockchain.get_height();
        debug!(
            "[{}] GetBlocks({}-{}) from {} (our height: {})",
            self.direction, start, end, self.peer_ip, our_height
        );

        // Check for repeated requests (loop detection)
        {
            let mut requests = self.recent_requests.write().await;
            let now = Instant::now();

            // Clean old requests (older than 30 seconds)
            requests.retain(|req| now.duration_since(req.timestamp) < Duration::from_secs(30));

            // Count similar requests in the last 30 seconds
            let similar_count = requests
                .iter()
                .filter(|req| {
                    // Consider requests similar if they overlap significantly
                    let start_match = (req.start as i64 - start as i64).abs() <= 100;
                    let end_match = (req.end as i64 - end as i64).abs() <= 100;
                    start_match && end_match
                })
                .count();

            if similar_count >= 20 {
                warn!(
                    "🚨 [{}] Possible sync loop detected: {} sent {} similar GetBlocks requests in 30s (ranges near {}-{}). Ignoring this request.",
                    self.direction, self.peer_ip, similar_count, start, end
                );
                if let Some(ai) = &context.ai_system {
                    ai.attack_detector.record_sync_flood(&self.peer_ip);
                }
                // Return no response — an empty BlocksResponse is indistinguishable from
                // "no blocks in that range" and causes the peer to retry immediately,
                // perpetuating the loop. Silence forces the peer to time out and back off.
                return Ok(None);
            }

            // Record this request
            requests.push(GetBlocksRequest {
                start,
                end,
                timestamp: now,
            });
        }

        let mut blocks = Vec::new();
        if start == 0 && end == 0 {
            match context.blockchain.get_block_by_height(0).await {
                Ok(genesis) => {
                    debug!(
                        "📤 [{}] Serving genesis block to {} via legacy GetBlocks(0,0)",
                        self.direction, self.peer_ip
                    );
                    return Ok(Some(NetworkMessage::BlocksResponse(vec![genesis])));
                }
                Err(e) => {
                    debug!(
                        "⚠️ [{}] Cannot serve legacy GetBlocks(0,0) to {} - no genesis yet: {}",
                        self.direction, self.peer_ip, e
                    );
                    return Ok(Some(NetworkMessage::BlocksResponse(Vec::new())));
                }
            }
        }
        // Send blocks we have: cap at our_height, requested end, and response limit.
        // Use MAX_BLOCKS_PER_RESPONSE (not SYNC_BATCH_SIZE) to ensure the serialized
        // response fits within the 8MB frame limit.
        let effective_end = end
            .min(start + crate::constants::network::MAX_BLOCKS_PER_RESPONSE - 1)
            .min(our_height);

        if start <= our_height {
            // CRITICAL: Only send contiguous blocks starting from requested start
            // Stop at first missing block to avoid sending incomplete ranges with gaps
            for h in start..=effective_end {
                match context.blockchain.get_block_by_height(h).await {
                    Ok(block) => blocks.push(block),
                    Err(e) => {
                        // Stop at first missing block - don't send partial ranges with gaps
                        warn!(
                            "⚠️ [{}] Missing block {} (stopping send to {} at height {}): {}",
                            self.direction,
                            h,
                            self.peer_ip,
                            h.saturating_sub(1),
                            e
                        );
                        break;
                    }
                }
            }

            if blocks.is_empty() && start <= our_height {
                warn!(
                    "⚠️ [{}] No blocks available to send to {} (requested {}-{}, our height: {}, missing block {})",
                    self.direction, self.peer_ip, start, end, our_height, start
                );
            } else if !blocks.is_empty() {
                let actual_start = blocks.first().unwrap().header.height;
                let actual_end = blocks.last().unwrap().header.height;
                debug!(
                    "📤 [{}] Sending {} blocks to {} (requested {}-{}, sending {}-{})",
                    self.direction,
                    blocks.len(),
                    self.peer_ip,
                    start,
                    end,
                    actual_start,
                    actual_end
                );
            }
        } else {
            // Requested blocks are beyond our height - we don't have them yet
            debug!(
                "[{}] Cannot send blocks {}-{} to {} - we only have up to height {}",
                self.direction, start, end, self.peer_ip, our_height
            );
        }

        // Ensure the serialized response fits within the frame limit.
        // 50 blocks is usually ~400KB but blocks with many transactions can
        // exceed 8MB. Trim from the end until it fits.
        let blocks = Self::trim_blocks_to_frame_limit(blocks);

        Ok(Some(NetworkMessage::BlocksResponse(blocks)))
    }

    /// Handle GetMasternodes request - respond with MasternodesResponse
    pub(super) async fn handle_get_block_height(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        debug!(
            "📥 [{}] Received GetBlockHeight from {}, responding with {}",
            self.direction, self.peer_ip, height
        );
        Ok(Some(NetworkMessage::BlockHeightResponse(height)))
    }

    /// Handle GetChainTip request
    pub(super) async fn handle_get_chain_tip(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let height = context.blockchain.get_height();
        let hash = match context.blockchain.get_block_hash(height) {
            Ok(h) => h,
            Err(e) => {
                // Log error but don't spam - this can happen during rapid block production
                tracing::debug!(
                    "[{}] Failed to get block hash at height {}: {} - using zero hash",
                    self.direction,
                    height,
                    e
                );
                [0u8; 32]
            }
        };
        // Only log at debug level to reduce noise
        tracing::debug!(
            "📥 [{}] GetChainTip from {}: height {} hash {}",
            self.direction,
            self.peer_ip,
            height,
            hex::encode(&hash[..8])
        );
        Ok(Some(NetworkMessage::ChainTipResponse { height, hash }))
    }

    /// Handle GetBlockRange request
    pub(super) async fn handle_get_block_range(
        &self,
        start_height: u64,
        end_height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Cap the range to MAX_BLOCKS_PER_RESPONSE regardless of what the peer asked for.
        // This bounds response size to ~400 KB compressed, keeping memory predictable on
        // small nodes (responses are never near MAX_FRAME_SIZE). Peers that need more
        // blocks issue additional GetBlockRange requests.
        let cap = crate::constants::network::MAX_BLOCKS_PER_RESPONSE;
        let capped_end = end_height.min(start_height.saturating_add(cap - 1));
        if capped_end < end_height {
            tracing::debug!(
                "📥 [{}] GetBlockRange({}-{}) from {} capped to {}-{}",
                self.direction,
                start_height,
                end_height,
                self.peer_ip,
                start_height,
                capped_end
            );
        } else {
            debug!(
                "📥 [{}] Received GetBlockRange({}-{}) from {}",
                self.direction, start_height, end_height, self.peer_ip
            );
        }
        let blocks = context
            .blockchain
            .get_block_range(start_height, capped_end)
            .await;
        // Ensure the serialized response fits within the frame limit.
        let blocks = Self::trim_blocks_to_frame_limit(blocks);
        Ok(Some(NetworkMessage::BlockRangeResponse(blocks)))
    }

    /// Handle GetBlockHash request
    pub(super) async fn handle_get_block_hash(
        &self,
        height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetBlockHash({}) from {}",
            self.direction, height, self.peer_ip
        );
        let hash = context.blockchain.get_block_hash_at_height(height).await;
        Ok(Some(NetworkMessage::BlockHashResponse { height, hash }))
    }

    /// Handle BlockRequest
    pub(super) async fn handle_block_request(
        &self,
        height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📨 [{}] Received block request for height {} from {}",
            self.direction, height, self.peer_ip
        );

        if let Ok(block) = context.blockchain.get_block_by_height(height).await {
            debug!(
                "✅ [{}] Sending block {} to {}",
                self.direction, height, self.peer_ip
            );
            Ok(Some(NetworkMessage::BlockResponse(block)))
        } else {
            debug!(
                "⚠️ [{}] Don't have block {} requested by {}",
                self.direction, height, self.peer_ip
            );
            Ok(None)
        }
    }

    /// Handle BlockInventory - request block if we need it
    pub(super) async fn handle_block_inventory(
        &self,
        block_height: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Reject inventory for blocks that couldn't possibly exist yet.
        // max_valid_height = 0 before genesis timestamp; blocks above that are pre-launch.
        let genesis_ts = context.blockchain.genesis_timestamp();
        let now = chrono::Utc::now().timestamp();
        let max_valid_height = if now >= genesis_ts {
            ((now - genesis_ts) / 600) as u64
        } else {
            0
        };
        if block_height > max_valid_height {
            warn!(
                "⏭️ [{}] Peer {} announced block {} before launch (max valid height: {}) — \
                 marking incompatible, keeping connection",
                self.direction, self.peer_ip, block_height, max_valid_height
            );
            context
                .peer_registry
                .mark_incompatible(
                    &self.peer_ip,
                    &format!(
                        "Announced pre-launch block {} (max valid height: {})",
                        block_height, max_valid_height
                    ),
                    false,
                )
                .await;
            self.suspend_peer_from_consensus(
                context,
                &format!("Pre-launch block announcement {}", block_height),
            )
            .await;
            return Ok(None);
        }

        let our_height = context.blockchain.get_height();

        if block_height > our_height {
            debug!(
                "📦 [{}] Received inventory for block {} from {}, requesting",
                self.direction, block_height, self.peer_ip
            );
            Ok(Some(NetworkMessage::BlockRequest(block_height)))
        } else {
            debug!(
                "⏭️ [{}] Ignoring inventory for block {} from {} (we're at {})",
                self.direction, block_height, self.peer_ip, our_height
            );
            Ok(None)
        }
    }

    /// Handle BlockResponse - add block to chain
    pub(super) async fn handle_block_response(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_height = block.header.height;

        // IBD guard: while far behind the expected tip, only accept blocks from
        // peers on the official whitelist (time-coin.io API, or addnode=
        // entries as fallback). Prevents a fresh node from latching onto a
        // forged fork served by a non-canonical peer.
        if self.should_drop_ibd_block(context).await {
            warn!(
                "🚫 [{}] Dropping block {} from non-canonical peer {} during initial block download (only whitelisted peers may serve historical blocks)",
                self.direction, block_height, self.peer_ip
            );
            return Ok(None);
        }

        // Check for duplicates using dedup filter if available
        if let Some(seen_blocks) = &context.seen_blocks {
            let block_height_bytes = block_height.to_le_bytes();
            if seen_blocks.check_and_insert(&block_height_bytes).await {
                debug!(
                    "🔁 [{}] Ignoring duplicate block {} from {}",
                    self.direction, block_height, self.peer_ip
                );
                return Ok(None);
            }
        }

        info!(
            "📥 [{}] Received block {} from {}",
            self.direction, block_height, self.peer_ip
        );

        match context
            .blockchain
            .add_block_with_fork_handling(block.clone())
            .await
        {
            Ok(true) => {
                info!(
                    "✅ [{}] Added block {} from {}",
                    self.direction, block_height, self.peer_ip
                );

                // Record block for AI predictive sync and transaction analysis
                if let Some(ai) = &context.ai_system {
                    let block_time = block.header.timestamp as u64;
                    ai.predictive_sync.record_block(
                        block_height,
                        block_time,
                        600, // nominal block time
                    );
                    let tx_count = block.transactions.len();
                    if tx_count > 0 {
                        tracing::debug!(
                            "📊 Block {} contains {} transactions",
                            block_height,
                            tx_count
                        );
                    }
                }

                // Gossip inventory to other peers
                if let Some(broadcast_tx) = &context.broadcast_tx {
                    let msg = NetworkMessage::BlockInventory(block_height);
                    if let Ok(receivers) = broadcast_tx.send(msg) {
                        debug!(
                            "🔄 [{}] Gossiped block {} inventory to {} peer(s)",
                            self.direction, block_height, receivers
                        );
                    }
                }
            }
            Ok(false) => {
                let current_height = context.blockchain.get_height();
                if block_height > current_height + 1 {
                    // Block is ahead of us — immediately request missing blocks
                    let gap = block_height - current_height - 1;
                    info!(
                        "📥 [{}] Block {} is ahead of our height {} (gap: {}) — requesting missing blocks from {}",
                        self.direction, block_height, current_height, gap, self.peer_ip
                    );
                    let sync_msg = NetworkMessage::GetBlocks(current_height + 1, block_height);
                    if let Err(e) = context
                        .peer_registry
                        .send_to_peer(&self.peer_ip, sync_msg)
                        .await
                    {
                        warn!("Failed to request missing blocks: {}", e);
                    }
                } else {
                    debug!(
                        "⏭️ [{}] Skipped block {} (already have or not sequential)",
                        self.direction, block_height
                    );
                }
            }
            Err(e) => {
                if e.contains("Fork detected") || e.contains("previous_hash") {
                    // Fork detected — trigger immediate resolution
                    warn!(
                        "🔀 [{}] Fork detected with {} at block {}: {}",
                        self.direction, self.peer_ip, block_height, e
                    );
                    let current_height = context.blockchain.get_height();

                    // Rate-limit: don't hammer the same peer with repeated fork-resolution
                    // requests. One request per 30 seconds per peer is sufficient.
                    let now_instant = std::time::Instant::now();
                    let recently_requested = context
                        .blockchain
                        .fork_resolution_last_request
                        .get(&self.peer_ip)
                        .map(|t| {
                            now_instant.duration_since(*t) < std::time::Duration::from_secs(30)
                        })
                        .unwrap_or(false);
                    if recently_requested {
                        debug!(
                            "⏭️  [{}] Fork resolution cooldown for {} — skipping repeat request",
                            self.direction, self.peer_ip
                        );
                    } else {
                        context
                            .blockchain
                            .fork_resolution_last_request
                            .insert(self.peer_ip.clone(), now_instant);
                        // Request blocks going back far enough to find common ancestor
                        let request_from = current_height.saturating_sub(20).max(1);
                        info!(
                            "📥 [{}] Requesting blocks {}-{} from {} for fork resolution",
                            self.direction, request_from, block_height, self.peer_ip
                        );
                        let sync_msg = NetworkMessage::GetBlocks(request_from, block_height);
                        if let Err(send_err) = context
                            .peer_registry
                            .send_to_peer(&self.peer_ip, sync_msg)
                            .await
                        {
                            warn!("Failed to request blocks for fork resolution: {}", send_err);
                        }
                    }
                } else if e.contains("unique reward recipient")
                    || e.contains("reward-hijacking")
                    || e.contains("reward_hijack")
                    || e.contains("under-subscribed genesis")
                    || e.contains("reward manipulation")
                    || e.contains("unknown masternodes")
                {
                    let is_whitelisted = context.peer_registry.is_whitelisted(&self.peer_ip).await;
                    if is_whitelisted {
                        warn!(
                            "⚠️ [{}] Reward mismatch on block {} from WHITELISTED peer {} — \
                             likely local registry divergence (our code may be outdated). \
                             Marking incompatible and keeping connection. Error: {}",
                            self.direction, block_height, self.peer_ip, e
                        );
                    } else {
                        warn!(
                            "⚠️ [{}] Invalid reward distribution in block {} from {} — \
                             marking incompatible, keeping connection: {}",
                            self.direction, block_height, self.peer_ip, e
                        );
                    }
                    // Mark peer incompatible so sync_from_peers stops selecting them,
                    // but keep the connection so they can heal (e.g. after upgrading code).
                    context
                        .peer_registry
                        .mark_incompatible(
                            &self.peer_ip,
                            &format!("Reward mismatch block {}: {}", block_height, e),
                            false, // not permanent — peer can reconnect after upgrading
                        )
                        .await;
                    self.suspend_peer_from_consensus(
                        context,
                        &format!("Reward mismatch block {}: {}", block_height, e),
                    )
                    .await;
                    return Ok(None);
                } else if e.contains("exceeds maximum expected height")
                    || e.contains("produced too early by")
                    || e.contains("predates network genesis")
                {
                    // Block predates the genesis launch window — peer is on old/pre-launch code.
                    // Keep the connection so they can heal after upgrading, but mark incompatible
                    // so we don't include them in quorum or sync selection.
                    warn!(
                        "⏭️ [{}] Pre-launch block {} from {} — marking incompatible, keeping connection ({})",
                        self.direction, block_height, self.peer_ip, e
                    );
                    context
                        .peer_registry
                        .mark_incompatible(
                            &self.peer_ip,
                            &format!("Sent pre-launch block {}: {}", block_height, e),
                            false,
                        )
                        .await;
                    self.suspend_peer_from_consensus(
                        context,
                        &format!("Pre-launch block {}: {}", block_height, e),
                    )
                    .await;
                    return Ok(None);
                } else {
                    warn!(
                        "❌ [{}] Failed to add block {}: {}",
                        self.direction, block_height, e
                    );
                }
            }
        }

        Ok(None)
    }

    /// Handle GetGenesisHash - respond with our genesis block hash
    pub(super) async fn handle_get_genesis_hash(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetGenesisHash from {}",
            self.direction, self.peer_ip
        );

        let genesis_hash = context.blockchain.genesis_hash();
        Ok(Some(NetworkMessage::GenesisHashResponse(genesis_hash)))
    }

    /// Handle GenesisHashResponse - verify peer's genesis matches ours
    pub(super) async fn handle_genesis_hash_response(
        &self,
        peer_genesis_hash: [u8; 32],
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_genesis_hash = context.blockchain.genesis_hash();

        // If we don't have genesis yet (all zeros), we can't compare
        if our_genesis_hash == [0u8; 32] {
            debug!(
                "[{}] We don't have genesis yet, cannot verify peer {} genesis hash",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        // If peer doesn't have genesis (all zeros), skip check
        if peer_genesis_hash == [0u8; 32] {
            debug!(
                "[{}] Peer {} doesn't have genesis yet, skipping verification",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        // Compare genesis hashes
        if our_genesis_hash == peer_genesis_hash {
            info!(
                "✅ [{}] Genesis hash verified with peer {} - compatible ({})",
                self.direction,
                self.peer_ip,
                hex::encode(&our_genesis_hash[..8])
            );
            // Mark peer as genesis-confirmed and reset any fork errors
            context.peer_registry.reset_fork_errors(&self.peer_ip);
            context
                .peer_registry
                .mark_genesis_confirmed(&self.peer_ip)
                .await;
            return Ok(None);
        }

        // Hashes differ.
        let our_height = context.blockchain.get_height();

        if our_height == 0 {
            // Still at genesis height — genesis convergence is still possible.
            // Request the peer's genesis block so we can compare hashes and keep
            // whichever is lower (deterministic tie-break; see replace_genesis_if_lower).
            info!(
                "🔀 [{}] Genesis hash differs from peer {} at height 0 — requesting \
                 their genesis for convergence (ours: {}, theirs: {})",
                self.direction,
                self.peer_ip,
                hex::encode(&our_genesis_hash[..8]),
                hex::encode(&peer_genesis_hash[..8])
            );
            return Ok(Some(
                crate::network::message::NetworkMessage::RequestGenesis,
            ));
        }

        // height > 0: we have blocks built on our genesis and are fully committed to it.
        warn!(
            "🚫 [{}] Genesis hash MISMATCH with peer {} - INCOMPATIBLE!",
            self.direction, self.peer_ip
        );
        warn!("   Our genesis:   {}", hex::encode(&our_genesis_hash[..8]));
        warn!("   Their genesis: {}", hex::encode(&peer_genesis_hash[..8]));

        // Mark peer as permanently incompatible
        context
            .peer_registry
            .mark_genesis_incompatible(
                &self.peer_ip,
                &hex::encode(&our_genesis_hash[..8]),
                &hex::encode(&peer_genesis_hash[..8]),
            )
            .await;
        self.suspend_peer_from_consensus(
            context,
            &format!(
                "Genesis hash mismatch: ours={}, theirs={}",
                hex::encode(&our_genesis_hash[..8]),
                hex::encode(&peer_genesis_hash[..8])
            ),
        )
        .await;

        // Permanently ban the peer in the IP banlist — a wrong genesis
        // means this peer is on a completely different chain and will never
        // be useful to us.
        self.permanent_ban_ip(
            context,
            &format!(
                "Genesis hash mismatch: ours={}, theirs={}",
                hex::encode(&our_genesis_hash[..8]),
                hex::encode(&peer_genesis_hash[..8])
            ),
        )
        .await;
        error!(
            "🚫 [AI] Permanently banned {} — wrong genesis block (theirs: {}, ours: {})",
            self.peer_ip,
            hex::encode(&peer_genesis_hash[..8]),
            hex::encode(&our_genesis_hash[..8])
        );

        Err(format!(
            "DISCONNECT: genesis hash mismatch (ours={}, theirs={})",
            hex::encode(&our_genesis_hash[..8]),
            hex::encode(&peer_genesis_hash[..8])
        ))
    }

    /// Handle RequestGenesis
    pub(super) async fn handle_request_genesis(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received genesis request from {}",
            self.direction, self.peer_ip
        );

        match context.blockchain.get_block_by_height(0).await {
            Ok(genesis) => {
                info!(
                    "📤 [{}] Sending genesis block to {}",
                    self.direction, self.peer_ip
                );
                Ok(Some(NetworkMessage::GenesisAnnouncement(genesis)))
            }
            Err(_) => {
                debug!(
                    "⚠️ [{}] Cannot fulfill genesis request - we don't have genesis yet",
                    self.direction
                );
                Ok(None)
            }
        }
    }

    /// Handle GenesisAnnouncement
    pub(super) async fn handle_genesis_announcement(
        &self,
        block: Block,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Verify this is actually a genesis block
        if block.header.height != 0 {
            warn!(
                "⚠️ [{}] Received GenesisAnnouncement for non-genesis block {} from {}",
                self.direction, block.header.height, self.peer_ip
            );
            return Ok(None);
        }

        info!(
            "📦 [{}] Received genesis announcement from {}",
            self.direction, self.peer_ip
        );

        // Check if we already have genesis
        if context.blockchain.get_block_by_height(0).await.is_ok() {
            let our_height = context.blockchain.get_height();

            if our_height > 0 {
                // We've built blocks on our genesis — we're committed.
                debug!(
                    "⏭️ [{}] Ignoring genesis announcement (chain at height {}) from {}",
                    self.direction, our_height, self.peer_ip
                );
                return Ok(None);
            }

            // height == 0: still in genesis election window.
            // Try to converge: replace ours if their hash is lower.
            match context
                .blockchain
                .replace_genesis_if_lower(block.clone())
                .await
            {
                Ok(true) => {
                    info!(
                        "🔀 [{}] Genesis replaced with lower-hash from {} — broadcasting",
                        self.direction, self.peer_ip
                    );
                    if let Some(broadcast_tx) = &context.broadcast_tx {
                        let _ = broadcast_tx.send(NetworkMessage::GenesisAnnouncement(block));
                    }
                }
                Ok(false) => {
                    // We kept our genesis (ours has lower or equal hash).
                    // If the peer is already past height 0 they are committed to a
                    // different chain and convergence is impossible — mark them
                    // genesis-incompatible so they stop skewing compare_chain_with_peers().
                    let peer_committed_height = context
                        .peer_registry
                        .get_peer_chain_tip(&self.peer_ip)
                        .await
                        .map(|(h, _)| h)
                        .unwrap_or(0);
                    if peer_committed_height > 0 {
                        warn!(
                            "🚫 [{}] Peer {} has different genesis and is committed at height {} \
                             — marking genesis-incompatible",
                            self.direction, self.peer_ip, peer_committed_height
                        );
                        let our_genesis = hex::encode(&context.blockchain.genesis_hash()[..8]);
                        context
                            .peer_registry
                            .mark_genesis_incompatible(
                                &self.peer_ip,
                                &our_genesis,
                                "committed_to_different_genesis",
                            )
                            .await;
                        self.suspend_peer_from_consensus(
                            context,
                            &format!(
                                "Committed to different genesis at height {}",
                                peer_committed_height
                            ),
                        )
                        .await;
                        // Keep the connection — they're excluded from quorum via
                        // mark_genesis_incompatible, but may still be useful for
                        // peer discovery. A genesis mismatch at this level is likely
                        // a fork or old code rather than a different chain entirely.
                        return Ok(None);
                    } else {
                        debug!(
                            "⏭️ [{}] Kept our genesis (already lower hash) — ignoring peer {}",
                            self.direction, self.peer_ip
                        );
                    }
                }
                Err(e) => {
                    // Wrong timestamp = peer is on a completely different genesis chain
                    // (e.g. old mainnet genesis timestamp 1775001600 vs our 1775001601).
                    // Mark them genesis-incompatible so they stop appearing in
                    // get_compatible_peers() and skewing compare_chain_with_peers().
                    if e.contains("timestamp") {
                        warn!(
                            "🚫 [{}] Peer {} genesis timestamp mismatch — marking genesis-incompatible: {}",
                            self.direction, self.peer_ip, e
                        );
                        let our_genesis = hex::encode(&context.blockchain.genesis_hash()[..8]);
                        context
                            .peer_registry
                            .mark_genesis_incompatible(
                                &self.peer_ip,
                                &our_genesis,
                                "wrong_timestamp",
                            )
                            .await;
                        self.suspend_peer_from_consensus(
                            context,
                            &format!("Genesis timestamp mismatch: {}", e),
                        )
                        .await;
                        // Keep the connection — genesis timestamp mismatch usually means
                        // they're on old code. Excluded from quorum via mark_genesis_incompatible.
                        return Ok(None);
                    } else {
                        warn!(
                            "⚠️ [{}] Candidate genesis from {} rejected: {}",
                            self.direction, self.peer_ip, e
                        );
                    }
                }
            }
            return Ok(None);
        }

        // Verify basic genesis structure
        use crate::block::genesis::GenesisBlock;
        match GenesisBlock::verify_structure(&block) {
            Ok(()) => {
                info!(
                    "✅ [{}] Genesis structure validation passed, adding to chain",
                    self.direction
                );

                match context.blockchain.add_block(block.clone()).await {
                    Ok(()) => {
                        info!("✅ [{}] Genesis block added successfully", self.direction);

                        // Broadcast to other peers
                        if let Some(broadcast_tx) = &context.broadcast_tx {
                            let msg = NetworkMessage::GenesisAnnouncement(block);
                            let _ = broadcast_tx.send(msg);
                        }
                    }
                    Err(e) => {
                        // Wrong timestamp from a peer we don't have genesis yet — mark incompatible
                        if e.contains("timestamp") {
                            warn!(
                                "🚫 [{}] Peer {} sent genesis with wrong timestamp — marking genesis-incompatible: {}",
                                self.direction, self.peer_ip, e
                            );
                            context
                                .peer_registry
                                .mark_genesis_incompatible(&self.peer_ip, "none", "wrong_timestamp")
                                .await;
                            self.suspend_peer_from_consensus(
                                context,
                                &format!("Wrong genesis timestamp: {}", e),
                            )
                            .await;
                        } else {
                            warn!("❌ [{}] Failed to add genesis block: {}", self.direction, e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ [{}] Genesis validation failed: {}", self.direction, e);
            }
        }

        Ok(None)
    }

    /// Handle TransactionBroadcast
    pub(super) async fn handle_chain_tip_response(
        &self,
        peer_height: u64,
        peer_hash: [u8; 32],
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let our_height = context.blockchain.get_height();
        let our_hash = context
            .blockchain
            .get_block_hash(our_height)
            .unwrap_or([0u8; 32]);

        // Update peer registry with their height and chain tip
        context
            .peer_registry
            .set_peer_height(&self.peer_ip, peer_height)
            .await;
        context
            .peer_registry
            .update_peer_chain_tip(&self.peer_ip, peer_height, peer_hash)
            .await;

        // Spawn background genesis verification for peers we haven't confirmed yet.
        // Skipped for:
        //   • peers already confirmed (same chain)
        //   • peers already permanently incompatible (genesis mismatch known — log once, done)
        //   • peers in the 5-minute cooldown after a timeout (old-code nodes that never respond)
        //   • peers with a concurrent check already in-flight (claim_genesis_check is atomic)
        if !context
            .peer_registry
            .is_genesis_confirmed(&self.peer_ip)
            .await
            && !context.peer_registry.is_incompatible(&self.peer_ip).await
            && context.peer_registry.claim_genesis_check(&self.peer_ip)
        {
            let registry = Arc::clone(&context.peer_registry);
            let peer_ip = self.peer_ip.clone();
            let our_genesis_hash = context
                .blockchain
                .get_block_by_height(0)
                .await
                .map(|b| b.hash())
                .unwrap_or([0u8; 32]);
            tokio::spawn(async move {
                registry
                    .verify_genesis_compatibility(&peer_ip, our_genesis_hash)
                    .await;
                registry.release_genesis_check(&peer_ip);
            });
        }

        tracing::debug!(
            "[{}] ChainTipResponse from {}: height {} hash {} (our height: {})",
            self.direction,
            self.peer_ip,
            peer_height,
            hex::encode(&peer_hash[..8]),
            our_height
        );

        if peer_height == our_height {
            // Same height - check if same hash (on same chain)
            if peer_hash != our_hash {
                // FORK DETECTED - same height but different blocks!
                // Rate-limit: only log once per 60s per peer to avoid flooding
                let now = Instant::now();
                let should_log = match fork_alert_rate_limit().get(&self.peer_ip) {
                    Some(entry) => now.duration_since(entry.0) >= Duration::from_secs(60),
                    None => true,
                };
                if should_log {
                    fork_alert_rate_limit().insert(self.peer_ip.clone(), (now, 0, peer_height));
                    warn!(
                        "🔀 [{}] FORK with {} at height {}: our {} vs their {}",
                        self.direction,
                        self.peer_ip,
                        peer_height,
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    );
                }

                // Check consensus - if we have majority, alert the peer
                // CRITICAL: Only count compatible peers (same genesis) for fork consensus
                let all_peers = context.peer_registry.get_compatible_peers().await;
                let mut our_chain_count = 1; // Count ourselves
                let mut peer_chain_count = 0;
                let mut counted_ips = std::collections::HashSet::new();

                for peer_addr in &all_peers {
                    if let Some((peer_h, p_hash)) =
                        context.peer_registry.get_peer_chain_tip(peer_addr).await
                    {
                        if peer_h == our_height {
                            let ip_only =
                                peer_addr.split(':').next().unwrap_or(peer_addr).to_string();
                            counted_ips.insert(ip_only);
                            if p_hash == our_hash {
                                our_chain_count += 1;
                            } else if p_hash == peer_hash {
                                peer_chain_count += 1;
                            }
                        }
                    }
                }

                // Supplement with recently-disconnected peer tips (same 5-min window
                // used by compare_chain_with_peers).  This prevents a minority-fork
                // node from falsely declaring "we have consensus" based on only 2 live
                // peers and sending erroneous ForkAlerts to canonical-chain peers,
                // which would trigger their AV30 counter and get us banned.
                const FORK_ALERT_RECENT_SECS: u64 = 300;
                let recent_tips = context
                    .peer_registry
                    .get_recent_chain_tips(FORK_ALERT_RECENT_SECS)
                    .await;
                for (tip_ip, tip_height, tip_hash) in &recent_tips {
                    let ip_only = tip_ip.split(':').next().unwrap_or(tip_ip).to_string();
                    if counted_ips.contains(&ip_only) {
                        continue; // already counted from live connection
                    }
                    if *tip_height == our_height {
                        counted_ips.insert(ip_only);
                        if *tip_hash == our_hash {
                            our_chain_count += 1;
                        } else if *tip_hash == peer_hash {
                            peer_chain_count += 1;
                        }
                    }
                }

                // If we have consensus and peer is on minority fork, send alert.
                // Require >= 3 distinct evidence sources (us + 2 others) before
                // declaring consensus — prevents a minority-fork node with only 2
                // live connections from falsely alerting canonical peers.
                if our_chain_count > peer_chain_count && our_chain_count >= 3 {
                    info!(
                        "📢 [{}] We have consensus ({} vs {} peers) at height {} - sending fork alert to {}",
                        self.direction, our_chain_count, peer_chain_count, peer_height, self.peer_ip
                    );

                    // Return ForkAlert message to be sent
                    return Ok(Some(NetworkMessage::ForkAlert {
                        your_height: peer_height,
                        your_hash: peer_hash,
                        consensus_height: our_height,
                        consensus_hash: our_hash,
                        consensus_peer_count: our_chain_count,
                        message: format!(
                            "You're on a minority fork at height {}. {} peers (including us) are on consensus chain with hash {}",
                            peer_height,
                            our_chain_count,
                            hex::encode(&our_hash[..8])
                        ),
                    }));
                }

                // At height 0 a hash mismatch means different genesis candidates.
                // Send RequestGenesis so handle_genesis_announcement can run the
                // lowest-hash convergence logic (replace_genesis_if_lower).
                // GetBlocks(0,5) is useless here — peers return empty responses
                // because block_0 is stored separately, not via GetBlocks.
                if peer_height == 0 {
                    info!(
                        "🔄 [{}] Requesting genesis from {} for height-0 convergence",
                        self.direction, self.peer_ip
                    );
                    return Ok(Some(NetworkMessage::RequestGenesis));
                }

                // Don't send a new GetBlocks request if fork resolution is already
                // in progress (FetchingChain / Reorging) — the state machine will
                // request exactly what it needs.  Sending a redundant GetBlocks here
                // while FetchingChain is active causes the response to be processed
                // through the normal add_block path, which re-detects the fork and
                // spawns a competing handle_fork(), resetting accumulated state and
                // creating a busy-loop that never reaches the common ancestor.
                // Also suppress during the finality-lock cooldown window so a blocked
                // reorg attempt cannot immediately re-trigger the full deep-fetch cycle.
                {
                    use crate::blockchain::ForkResolutionState;
                    use std::sync::atomic::Ordering;
                    let fs = context.blockchain.fork_state.read().await;
                    if !matches!(*fs, ForkResolutionState::None) {
                        debug!(
                            "⏭️  [{}] Skipping GetBlocks to {} — fork resolution already active",
                            self.direction, self.peer_ip
                        );
                        return Ok(None);
                    }
                    drop(fs);
                    let now_secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let blocked_until = context
                        .blockchain
                        .fork_resolution_blocked_until
                        .load(Ordering::Acquire);
                    if now_secs < blocked_until {
                        debug!(
                            "⏭️  [{}] Skipping GetBlocks to {} — finality-lock cooldown \
                             ({}s remaining)",
                            self.direction,
                            self.peer_ip,
                            blocked_until.saturating_sub(now_secs)
                        );
                        return Ok(None);
                    }
                }

                // Rate-limit: one fork resolution request per peer per 30 seconds.
                let now_instant = std::time::Instant::now();
                let recently_requested = context
                    .blockchain
                    .fork_resolution_last_request
                    .get(&self.peer_ip)
                    .map(|t| now_instant.duration_since(*t) < std::time::Duration::from_secs(30))
                    .unwrap_or(false);
                if recently_requested {
                    debug!(
                        "⏭️  [{}] Fork resolution cooldown for {} — skipping repeat request",
                        self.direction, self.peer_ip
                    );
                    return Ok(None);
                }
                context
                    .blockchain
                    .fork_resolution_last_request
                    .insert(self.peer_ip.clone(), now_instant);
                let request_from = peer_height.saturating_sub(10);
                info!(
                    "🔄 [{}] Requesting blocks {}-{} from {} for fork resolution",
                    self.direction,
                    request_from,
                    peer_height + 5,
                    self.peer_ip
                );
                return Ok(Some(NetworkMessage::GetBlocks(
                    request_from,
                    peer_height + 5,
                )));
            } else {
                debug!(
                    "✅ [{}] Peer {} on same chain at height {}",
                    self.direction, self.peer_ip, peer_height
                );
                // Peer caught up — clear zombie timer if any
                zombie_peer_tracker().remove(&self.peer_ip);
            }
        } else if peer_height > our_height {
            // Peer is ahead — clear zombie timer (they're clearly syncing)
            zombie_peer_tracker().remove(&self.peer_ip);
            // Peer is ahead — accept blocks from compatible peers at any gap.
            // Block validation (reward structure, VRF, etc.) is the real safety gate:
            // if a peer sends invalid blocks they get banned. The old "reject if gap 6-10
            // and not in consensus" rule caused a deadlock where a compatible peer that had
            // advanced past us could neither have its blocks accepted NOR allow us to produce
            // (because fork-prevention skips production when any compatible peer is ahead).
            let height_gap = peer_height - our_height;
            if height_gap > 5 {
                let is_consensus_peer =
                    context.blockchain.is_peer_in_consensus(&self.peer_ip).await;
                if !is_consensus_peer {
                    debug!(
                        "🔓 [{}] Accepting blocks from {} despite non-consensus (gap {}, compatible peer — block validation is the safety gate)",
                        self.direction, self.peer_ip, height_gap
                    );
                }
            }

            // Peer is ahead and in consensus - sync from them.
            // Rate-limit to one GetBlocks request per peer per 60 s to avoid
            // triggering the remote peer's sync-loop detector when block
            // announcements arrive faster than the sync can complete.
            {
                let now = Instant::now();
                let should_request = match chain_tip_getblocks_rate_limit().get(&self.peer_ip) {
                    Some(last) => now.duration_since(*last) >= Duration::from_secs(60),
                    None => true,
                };
                if !should_request {
                    debug!(
                        "📈 [{}] Peer {} ahead at height {} — GetBlocks rate-limited (wait 60s)",
                        self.direction, self.peer_ip, peer_height
                    );
                    return Ok(None);
                }
                chain_tip_getblocks_rate_limit().insert(self.peer_ip.clone(), now);
            }
            debug!(
                "📈 [{}] Peer {} ahead at height {} (we have {}), requesting blocks",
                self.direction, self.peer_ip, peer_height, our_height
            );
            return Ok(Some(NetworkMessage::GetBlocks(
                our_height + 1,
                peer_height + 1,
            )));
        } else {
            // We're ahead - peer might need to sync from us
            let height_diff = our_height - peer_height;

            // Rate-limit fork alerts with exponential backoff: 60s → 2m → 5m → 10m cap.
            // Resets when the peer's height changes (i.e., they're making progress).
            if height_diff >= 2 {
                let now = Instant::now();
                let (should_alert, alert_count) = match fork_alert_rate_limit().get(&self.peer_ip) {
                    Some(entry) => {
                        let (last_time, count, last_height) = *entry;
                        if last_height != peer_height {
                            // Peer height changed — they're syncing, reset backoff
                            (true, 0u32)
                        } else {
                            // Exponential backoff: 60s, 120s, 300s, 600s cap
                            let interval = match count {
                                0 => 60,
                                1 => 120,
                                2 => 300,
                                _ => 600,
                            };
                            (
                                now.duration_since(last_time) >= Duration::from_secs(interval),
                                count,
                            )
                        }
                    }
                    None => (true, 0u32),
                };

                // Don't send fork alerts while we are still syncing: we haven't
                // seen the full chain yet, so we are not a reliable consensus
                // authority.  Our "our_height" is a local minimum, not a network
                // consensus view.
                if should_alert && !context.blockchain.is_syncing() {
                    let all_peers = context.peer_registry.get_compatible_peers().await;
                    let mut our_chain_count: usize = 1; // Count ourselves
                    let mut behind_count: usize = 0;

                    for peer_addr in &all_peers {
                        if let Some((peer_h, _)) =
                            context.peer_registry.get_peer_chain_tip(peer_addr).await
                        {
                            if peer_h >= our_height {
                                our_chain_count += 1;
                            } else if peer_h <= peer_height {
                                behind_count += 1;
                            }
                        }
                    }

                    if our_chain_count >= 3 && our_chain_count > behind_count {
                        let new_count = alert_count + 1;
                        fork_alert_rate_limit()
                            .insert(self.peer_ip.clone(), (now, new_count, peer_height));
                        let next_interval = match new_count {
                            0..=1 => 120,
                            2 => 300,
                            _ => 600,
                        };
                        info!(
                            "📢 [{}] Peer {} is {} blocks behind (height {}). Consensus: {} peers at height {}. Sending sync alert (#{}, next in {}s).",
                            self.direction, self.peer_ip, height_diff, peer_height,
                            our_chain_count, our_height, new_count, next_interval
                        );
                        return Ok(Some(NetworkMessage::ForkAlert {
                            your_height: peer_height,
                            your_hash: peer_hash,
                            consensus_height: our_height,
                            consensus_hash: our_hash,
                            consensus_peer_count: our_chain_count,
                            message: format!(
                                "You're behind at height {} while {} peers are at height {}. Please sync.",
                                peer_height, our_chain_count, our_height
                            ),
                        }));
                    }
                }
            }

            debug!(
                "📉 [{}] Peer {} behind at height {} (we have {})",
                self.direction, self.peer_ip, peer_height, our_height
            );

            // Zombie peer eviction: if a peer has been >=200 blocks behind
            // for longer than ZOMBIE_TIMEOUT, it is stuck and will never catch
            // up on its own.  Return a DISCONNECT error so the message loop
            // breaks and the standard disconnect-cleanup path handles teardown.
            //
            // We do NOT call kick_peer() here — that caused a double-cleanup
            // stall where kick_peer acquired peer_writers.write() and then the
            // disconnect path's unregister_peer acquired it again, freezing
            // all network I/O.  The DISCONNECT error alone is sufficient:
            // the message loop breaks, PeerConnection drops its writer_tx
            // clone, and the normal cleanup runs exactly once.
            if height_diff >= 200 {
                let now = Instant::now();
                let since = *zombie_peer_tracker()
                    .entry(self.peer_ip.clone())
                    .or_insert(now);
                if now.duration_since(since) >= ZOMBIE_TIMEOUT {
                    let reason = format!(
                        "zombie: {} blocks behind for >{:.0}s",
                        height_diff,
                        ZOMBIE_TIMEOUT.as_secs_f32(),
                    );
                    warn!(
                        "🧟 [{}] Peer {} is zombie ({}) — marking incompatible, keeping connection so it can heal",
                        self.direction, self.peer_ip, reason,
                    );
                    zombie_peer_tracker().remove(&self.peer_ip);
                    context
                        .peer_registry
                        .mark_incompatible(&self.peer_ip, &reason, false)
                        .await;
                    self.suspend_peer_from_consensus(context, &reason).await;
                    // Don't disconnect — peer may catch up after upgrading or finding better peers.
                    // Excluded from quorum/sync via mark_incompatible.
                    return Ok(None);
                }
            } else {
                // Peer made progress — clear any zombie timer
                zombie_peer_tracker().remove(&self.peer_ip);
            }
        }

        Ok(None)
    }

    /// Handle BlocksResponse/BlockRangeResponse - centralized block processing
    ///
    /// This replaces the duplicated logic that was in peer_connection.rs
    pub(super) async fn handle_blocks_response(
        &self,
        blocks: Vec<Block>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let block_count = blocks.len();
        if block_count == 0 {
            info!(
                "📥 [{}] Received empty blocks response from {}",
                self.direction, self.peer_ip
            );
            return Ok(None);
        }

        let start_height = blocks.first().map(|b| b.header.height).unwrap_or(0);
        let end_height = blocks.last().map(|b| b.header.height).unwrap_or(0);

        // Check if peer is whitelisted
        let is_whitelisted = context.peer_registry.is_whitelisted(&self.peer_ip).await;

        // IBD guard: while far behind the expected tip, only accept block
        // batches from peers on the official whitelist (time-coin.io API, or
        // addnode= entries as fallback). Prevents a fresh node from syncing a
        // forged fork from a non-canonical peer.
        if self.should_drop_ibd_block(context).await {
            warn!(
                "🚫 [{}] Dropping {} blocks (height {}-{}) from non-canonical peer {} during initial block download (only whitelisted peers may serve historical blocks)",
                self.direction, block_count, start_height, end_height, self.peer_ip
            );
            return Ok(None);
        }

        info!(
            "📥 [{}] Received {} blocks (height {}-{}) from {} {}",
            self.direction,
            block_count,
            start_height,
            end_height,
            self.peer_ip,
            if is_whitelisted { "(whitelisted)" } else { "" }
        );

        // Check if we're in fork resolution state - if so, route blocks to fork handler
        {
            use crate::blockchain::ForkResolutionState;
            let fork_state = context.blockchain.fork_state.read().await;
            if let ForkResolutionState::FetchingChain { peer_addr, .. } = &*fork_state {
                if peer_addr == &self.peer_ip {
                    info!(
                        "📥 [{}] Received blocks from {} match active fork resolution - routing to handle_fork()",
                        self.direction, self.peer_ip
                    );
                    drop(fork_state); // Release lock before async call

                    spawn_fork_resolution(
                        context.blockchain.clone(),
                        blocks,
                        self.peer_ip.clone(),
                        context.banlist.clone(),
                        context.peer_registry.clone(),
                        context.masternode_registry.clone(),
                    );

                    return Ok(None);
                }
            }
        }

        // Try to add blocks sequentially, buffering any that are ahead of our tip
        let mut added = 0;
        let mut buffered = 0;
        let mut skipped = 0;
        let mut fork_detected = false;

        for block in blocks.iter() {
            // Validate block has non-zero previous_hash (except genesis at height 0)
            if block.header.height > 0 && block.header.previous_hash == [0u8; 32] {
                warn!(
                    "⚠️ [{}] Peer {} sent corrupt block {} with zero previous_hash - skipping",
                    self.direction, self.peer_ip, block.header.height
                );
                skipped += 1;
                if is_whitelisted {
                    warn!(
                        "⚠️ [{}] Whitelisted peer {} sent corrupt block - data quality issue!",
                        self.direction, self.peer_ip
                    );
                }
                continue;
            }

            // If block is ahead of our tip + 1, buffer it for later application
            let current_height = context.blockchain.get_height();
            if block.header.height > current_height + 1 {
                if context.blockchain.buffer_sync_block(block.clone()).await {
                    buffered += 1;
                    debug!(
                        "📦 [{}] Buffered ahead-of-tip block {} from {} (our height: {})",
                        self.direction, block.header.height, self.peer_ip, current_height
                    );
                }
                continue;
            }

            // CRITICAL: Run block processing on a blocking thread so synchronous
            // sled I/O doesn't starve tokio worker threads. Without this, every
            // sled read/write (save_block, get_block, update_height, undo_log)
            // blocks a worker thread, and with enough concurrent operations ALL
            // workers get stuck — killing RPC, timers, and networking.
            let blockchain = context.blockchain.clone();
            let block_clone = block.clone();
            let result = tokio::task::spawn_blocking(move || {
                tokio::runtime::Handle::current()
                    .block_on(async { blockchain.add_block_with_fork_handling(block_clone).await })
            })
            .await;

            // Unwrap the JoinError from spawn_blocking, then handle the inner Result
            let result = match result {
                Ok(inner) => inner,
                Err(e) => {
                    warn!(
                        "❌ [{}] Block processing task panicked for block {} from {}: {}",
                        self.direction, block.header.height, self.peer_ip, e
                    );
                    Err(format!("Block processing panicked: {}", e))
                }
            };

            match result {
                Ok(true) => {
                    added += 1;

                    // Reset persistent fork error counter on successful block
                    context.peer_registry.reset_fork_errors(&self.peer_ip);

                    // Clear incompatible status if blocks now work
                    if added == 1 {
                        context
                            .peer_registry
                            .clear_incompatible(&self.peer_ip)
                            .await;
                        self.clear_peer_consensus_suspension(context).await;
                    }
                }
                Ok(false) => {
                    // Block already exists or is not next in chain
                    debug!(
                        "⏭️ [{}] Skipped block {} from {} (already exists or not sequential)",
                        self.direction, block.header.height, self.peer_ip
                    );
                    skipped += 1;
                }
                Err(e)
                    if e.contains("Fork detected")
                        || e.contains("previous_hash")
                        || e.contains("incorrect block_reward")
                        || e.contains("pool theft") =>
                {
                    fork_detected = true;
                    skipped += 1;

                    debug!(
                        "🔀 [{}] Fork/divergence detected from {}: {}",
                        self.direction, self.peer_ip, e
                    );

                    // If block 1 has a prev_hash that doesn't match our genesis, the peer
                    // is on a different chain.  Request their genesis so:
                    //  - old-chain nodes (wrong timestamp) get marked genesis-incompatible
                    //    and are excluded from compare_chain_with_peers().
                    //  - new-chain nodes (correct timestamp, different hash) are handled
                    //    by replace_genesis_if_lower() convergence.
                    if block.header.height == 1
                        && context.blockchain.get_height() == 0
                        && e.contains("prev_hash")
                    {
                        info!(
                            "🔄 [{}] Block 1 from {} has wrong prev_hash — requesting their genesis \
                             to determine compatibility",
                            self.direction, self.peer_ip
                        );
                        return Ok(Some(
                            crate::network::message::NetworkMessage::RequestGenesis,
                        ));
                    }

                    // Track fork errors (for metrics/debugging)
                    let _error_count = context.peer_registry.increment_fork_errors(&self.peer_ip);
                    if let Some(ai) = &context.ai_system {
                        ai.attack_detector.record_fork(&self.peer_ip);
                    }

                    // IMMEDIATE fork resolution - don't wait for multiple errors
                    // If we detect a fork, we need to resolve it right away
                    warn!(
                        "🔀 [{}] Fork detected with peer {} at height {}: {}",
                        self.direction, self.peer_ip, block.header.height, e
                    );

                    // Only engage fork resolution with genesis-confirmed peers.
                    // Old-code nodes that do not respond to GetBlockHash(0) are never
                    // genesis-confirmed, so they cannot trigger endless reorg loops.
                    // Exception: whitelisted (operator-trusted) peers bypass the gate to
                    // avoid a race where fork detection fires before the background
                    // genesis-verification task completes (~10s window).
                    if !context
                        .peer_registry
                        .is_genesis_confirmed(&self.peer_ip)
                        .await
                    {
                        let is_whitelisted =
                            context.peer_registry.is_whitelisted(&self.peer_ip).await;
                        if is_whitelisted {
                            // Trust operator's whitelist; mark confirmed so future checks pass.
                            context
                                .peer_registry
                                .mark_genesis_confirmed(&self.peer_ip)
                                .await;
                            info!(
                                "🔓 [{}] Whitelisted peer {} not yet genesis-confirmed — bypassing gate and marking confirmed",
                                self.direction, self.peer_ip
                            );
                        } else {
                            warn!(
                                "🚫 [{}] Skipping fork resolution with {} — peer not genesis-confirmed (likely old code)",
                                self.direction, self.peer_ip
                            );
                            break;
                        }
                    }

                    // Trigger immediate fork resolution check
                    info!(
                        "🔄 [{}] Fork with {} - initiating immediate resolution",
                        self.direction, self.peer_ip
                    );

                    spawn_fork_resolution(
                        context.blockchain.clone(),
                        blocks.to_vec(),
                        self.peer_ip.clone(),
                        context.banlist.clone(),
                        context.peer_registry.clone(),
                        context.masternode_registry.clone(),
                    );

                    // Stop processing remaining blocks - let fork resolution handle it
                    break;
                }
                Err(e)
                    if e.contains("unique reward recipient")
                        || e.contains("reward-hijacking")
                        || e.contains("reward_hijack")
                        || e.contains("under-subscribed genesis")
                        || e.contains("unknown recipient")
                        || e.contains("exceeds max tier pool")
                        || e.contains("reward manipulation")
                        || e.contains("unknown masternodes") =>
                {
                    let block_height = block.header.height;
                    if block_height <= 1 {
                        warn!(
                            "🛡️ [{}] Block {} from {} has invalid reward distribution (likely bootstrap race) — soft-marking incompatible: {}",
                            self.direction, block_height, self.peer_ip, e
                        );
                        context
                            .peer_registry
                            .mark_incompatible(
                                &self.peer_ip,
                                &format!(
                                    "Bad block {} reward (bootstrap race): {}",
                                    block_height, e
                                ),
                                false,
                            )
                            .await;
                        self.suspend_peer_from_consensus(
                            context,
                            &format!("Bad block {} reward (bootstrap race): {}", block_height, e),
                        )
                        .await;
                    } else {
                        // Peer sent a block with invalid reward distribution — likely old code
                        // or on a fork. Mark incompatible so we don't use them for quorum/sync,
                        // but keep the connection so they can heal after upgrading.
                        warn!(
                            "⚠️ [{}] Block {} from {} has invalid reward distribution — \
                             marking incompatible, keeping connection: {}",
                            self.direction, block_height, self.peer_ip, e
                        );
                        context
                            .peer_registry
                            .mark_incompatible(
                                &self.peer_ip,
                                &format!("Bad block {} reward: {}", block_height, e),
                                false, // not permanent — peer may fix after upgrade
                            )
                            .await;
                        self.suspend_peer_from_consensus(
                            context,
                            &format!("Bad block {} reward: {}", block_height, e),
                        )
                        .await;
                    }
                    // Don't disconnect — peer is excluded from quorum/sync via mark_incompatible.
                    return Ok(None);
                }
                Err(e) if e.contains("corrupted") || e.contains("serialization failed") => {
                    // SECURITY: Corrupted block is a SEVERE violation - potential attack
                    error!(
                        "🚨 [{}] CORRUPTED BLOCK {} from {} - potential attack: {}",
                        self.direction, block.header.height, self.peer_ip, e
                    );

                    // Record severe violation and potentially ban the peer
                    if self.peer_ip.parse::<std::net::IpAddr>().is_ok() {
                        // Mark peer as incompatible - they have corrupted data
                        // Corrupted blocks are temporary (might be software bug, not permanent)
                        context
                            .peer_registry
                            .mark_incompatible(
                                &self.peer_ip,
                                &format!("Sent corrupted block {}: {}", block.header.height, e),
                                false, // temporary - will be rechecked
                            )
                            .await;
                        self.suspend_peer_from_consensus(
                            context,
                            &format!("Sent corrupted block {}: {}", block.header.height, e),
                        )
                        .await;
                    }

                    // Stop processing ALL blocks from this peer in this batch
                    warn!(
                        "🚫 [{}] Rejecting all {} blocks from {} due to corruption",
                        self.direction, block_count, self.peer_ip
                    );
                    return Err(format!(
                        "Peer {} sent corrupted block - connection should be terminated",
                        self.peer_ip
                    ));
                }
                Err(e)
                    if e.contains("exceeds maximum expected height")
                        || e.contains("produced too early by")
                        || e.contains("predates network genesis") =>
                {
                    // Block predates the genesis launch window — peer is on old/pre-launch code.
                    // Mark incompatible and stop processing this batch, but keep the connection
                    // so they can heal after upgrading.
                    warn!(
                        "⏭️ [{}] Pre-launch block {} from {} — marking incompatible, keeping connection: {}",
                        self.direction, block.header.height, self.peer_ip, e
                    );
                    context
                        .peer_registry
                        .mark_incompatible(
                            &self.peer_ip,
                            &format!(
                                "Pre-launch block batch (height {}): {}",
                                block.header.height, e
                            ),
                            false,
                        )
                        .await;
                    self.suspend_peer_from_consensus(
                        context,
                        &format!(
                            "Pre-launch block batch (height {}): {}",
                            block.header.height, e
                        ),
                    )
                    .await;
                    break; // Stop processing this batch; connection stays open
                }
                Err(e) => {
                    warn!(
                        "❌ [{}] Failed to add block {} from {}: {}",
                        self.direction, block.header.height, self.peer_ip, e
                    );
                    skipped += 1;
                }
            }
        }

        if added > 0 {
            // Flush storage after processing the batch — during sync, per-block
            // flushes are skipped to avoid blocking the tokio runtime with fsync.
            // flush_storage_async uses spawn_blocking internally.
            if let Err(e) = context.blockchain.flush_storage_async().await {
                warn!("⚠️ [{}] Post-batch flush failed: {}", self.direction, e);
            }

            // After successfully adding blocks, drain any buffered blocks that are now sequential
            let drained = context.blockchain.drain_pending_blocks().await;
            if drained > 0 {
                added += drained as usize;
                info!(
                    "📦 [{}] Drained {} buffered blocks after batch from {}",
                    self.direction, drained, self.peer_ip
                );
            }

            let pending = context.blockchain.pending_block_count().await;
            info!(
                "✅ [{}] Added {} blocks from {} (skipped {}, buffered {}, pending {})",
                self.direction, added, self.peer_ip, skipped, buffered, pending
            );
        } else if (skipped > 0 || buffered > 0) && !fork_detected {
            if buffered > 0 {
                // Even when no blocks were added directly, try draining the buffer.
                // During parallel sync, all received blocks may be ahead-of-tip
                // (added == 0) but another peer's response may have already filled
                // the gap. Without this drain, the buffer grows indefinitely and
                // the node appears stuck.
                let drained = context.blockchain.drain_pending_blocks().await;
                if drained > 0 {
                    if let Err(e) = context.blockchain.flush_storage_async().await {
                        warn!("⚠️ [{}] Post-drain flush failed: {}", self.direction, e);
                    }
                    let pending = context.blockchain.pending_block_count().await;
                    info!(
                        "✅ [{}] Drained {} buffered blocks after batch from {} ({} pending remaining)",
                        self.direction, drained, self.peer_ip, pending
                    );
                } else {
                    let pending = context.blockchain.pending_block_count().await;
                    info!(
                        "📦 [{}] Buffered {} blocks from {} for parallel sync ({} pending total)",
                        self.direction, buffered, self.peer_ip, pending
                    );
                }
            } else {
                // No blocks added or buffered
                let current_height = context.blockchain.get_height();
                warn!(
                    "⚠️ [{}] No blocks added from {} - all {} blocks skipped (our height {})",
                    self.direction, self.peer_ip, skipped, current_height
                );
            }
        }

        if fork_detected {
            warn!(
                "⚠️ [{}] All {} blocks skipped from {} (fork detected)",
                self.direction, block_count, self.peer_ip
            );

            // ── AV30: Record rejected fork-alert cycle ───────────────────────
            // Only count as a "rejected cycle" for peers that are NOT
            // genesis-confirmed.  A genesis-confirmed peer is on the same chain
            // (same genesis block) but a different branch — that is recoverable
            // fork divergence, not a fork-bomb attack.  Counting it here would
            // eventually ban legitimate canonical-chain peers when we are the
            // node on the minority fork, creating the self-perpetuating isolation
            // cycle the user observed.  AV30 protection is reserved for peers
            // whose genesis is unknown or different (potential attackers).
            let peer_is_genesis_confirmed = context
                .peer_registry
                .is_genesis_confirmed(&self.peer_ip)
                .await;
            if !peer_is_genesis_confirmed {
                let now = Instant::now();
                let tracker = incoming_fork_alert_tracker();
                let mut entry = tracker
                    .entry(self.peer_ip.clone())
                    .or_insert_with(|| (now, 0u32, now));
                let (last_sent, rejected_cycles, window_start) = *entry;
                let in_window = now.duration_since(window_start) < FORK_ALERT_WINDOW;
                let new_cycles = if in_window { rejected_cycles + 1 } else { 1 };
                let new_window = if in_window { window_start } else { now };
                *entry = (last_sent, new_cycles, new_window);
                debug!(
                    "🔄 [AV30] Recorded rejected fork-alert cycle {}/{} for {} (unconfirmed genesis)",
                    new_cycles, FORK_ALERT_BAN_THRESHOLD, self.peer_ip
                );
            } else {
                debug!(
                    "🔀 [AV30] Fork divergence from genesis-confirmed peer {} — skipping AV30 (legitimate fork, not attack)",
                    self.peer_ip
                );
            }
            // ── End AV30 cycle recording ─────────────────────────────────────
        }

        Ok(None)
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - Message Handlers
    // ========================================================================

    /// Handle LivenessAlert message (§7.6.2)
    /// Node receives alert that a transaction has stalled
    pub(super) fn validate_block_rewards_structure(&self, block: &Block) -> Result<(), String> {
        // Transaction 0 should be coinbase
        let coinbase = &block.transactions[0];
        if !coinbase.inputs.is_empty() {
            return Err(format!(
                "Coinbase has {} inputs, expected 0",
                coinbase.inputs.len()
            ));
        }

        if coinbase.outputs.len() != 1 {
            return Err(format!(
                "Coinbase has {} outputs, expected 1",
                coinbase.outputs.len()
            ));
        }

        let coinbase_amount = coinbase.outputs[0].value;
        if coinbase_amount != block.header.block_reward {
            return Err(format!(
                "Coinbase creates {} satoshis, but block_reward is {}",
                coinbase_amount, block.header.block_reward
            ));
        }

        // Transaction 1 should be reward distribution
        let reward_dist = &block.transactions[1];

        if reward_dist.inputs.len() != 1 {
            return Err(format!(
                "Reward distribution has {} inputs, expected 1",
                reward_dist.inputs.len()
            ));
        }

        let coinbase_txid = coinbase.txid();
        if reward_dist.inputs[0].previous_output.txid != coinbase_txid {
            return Err("Reward distribution doesn't spend coinbase".to_string());
        }

        // Output count may differ from masternode_rewards count when multiple
        // masternodes share a reward address (entries are merged in newer code).
        // Only reject if outputs exceed metadata entries (more outputs than expected).
        if reward_dist.outputs.len() > block.masternode_rewards.len() {
            return Err(format!(
                "Reward distribution has {} outputs but masternode_rewards has only {} entries",
                reward_dist.outputs.len(),
                block.masternode_rewards.len()
            ));
        }

        // Verify total outputs match block reward exactly (with small tolerance for rounding)
        let total_distributed: u64 = reward_dist.outputs.iter().map(|o| o.value).sum();
        let expected_total = block.header.block_reward;

        // Allow small tolerance for rounding errors in integer division
        // Tolerance should be less than the number of masternodes (worst case: 1 satoshi per node)
        let tolerance = block.masternode_rewards.len() as u64;

        let lower_bound = expected_total.saturating_sub(tolerance);
        let upper_bound = expected_total;

        if total_distributed < lower_bound || total_distributed > upper_bound {
            return Err(format!(
                "Total distributed {} outside valid range {}-{} (block_reward: {})",
                total_distributed, lower_bound, upper_bound, expected_total
            ));
        }

        Ok(())
    }
}
