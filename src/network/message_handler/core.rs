use super::common::{check_sliding_window, split_utxos_into_chunks};
use super::context::MessageContext;
use super::ConnectionDirection;
use crate::block::types::Block;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::message::NetworkMessage;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// Tracks repeated GetBlocks requests to detect loops
#[derive(Debug, Clone)]
pub(super) struct GetBlocksRequest {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) timestamp: Instant,
}

/// Unified message handler for all network messages
pub struct MessageHandler {
    pub(super) peer_ip: String,
    pub(super) direction: ConnectionDirection,
    pub(super) recent_requests: Arc<RwLock<Vec<GetBlocksRequest>>>,
    /// AV27: sliding window for invalid-vote-signature rejection counts.
    /// Tracks (count, window_start). When count reaches 5 within 30 s, one
    /// violation is recorded against the peer and the counter resets.
    /// Sliding window prevents in-flight stale votes around block transitions
    /// from triggering bans on legitimate peers.
    pub(super) invalid_sig_vote_window: Arc<Mutex<(u32, Instant)>>,
    /// AV28: sliding window for unregistered-voter vote rejection counts.
    /// Tracks (count, window_start). When count reaches 10 within 60 s, one
    /// violation is recorded against the peer and the counter resets.
    pub(super) unregistered_vote_window: Arc<Mutex<(u32, Instant)>>,
}

impl MessageHandler {
    /// Create a new message handler for a specific peer and connection direction
    pub fn new(peer_ip: String, direction: ConnectionDirection) -> Self {
        Self {
            peer_ip,
            direction,
            recent_requests: Arc::new(RwLock::new(Vec::new())),
            invalid_sig_vote_window: Arc::new(Mutex::new((0, Instant::now()))),
            unregistered_vote_window: Arc::new(Mutex::new((0, Instant::now()))),
        }
    }

    /// Gap (in blocks) below which we no longer treat the node as "in IBD".
    /// While syncing, we restrict block sources to the official time-coin.io
    /// peer list to prevent latching onto a forged fork from a rogue peer.
    const IBD_GAP_THRESHOLD: u64 = 10;

    /// True when the local chain is far enough behind the expected tip that we
    /// should treat incoming blocks as initial-block-download traffic.
    fn is_initial_block_download(&self, context: &MessageContext) -> bool {
        let current = context.blockchain.get_height();
        let expected = context.blockchain.calculate_expected_height();
        expected.saturating_sub(current) > Self::IBD_GAP_THRESHOLD
    }

    /// During IBD, drop blocks from peers not on the official whitelist
    /// (time-coin.io API or addnode= fallback). Returns true if the block
    /// should be dropped. If the whitelist is empty (API unreachable AND no
    /// addnode entries), the guard is disabled so the node can still sync.
    /// Genesis-confirmed peers bypass the whitelist check — a verified genesis
    /// hash is a stronger same-chain guarantee than the API list alone.
    pub(super) async fn should_drop_ibd_block(&self, context: &MessageContext) -> bool {
        if !self.is_initial_block_download(context) {
            return false;
        }
        if context
            .peer_registry
            .is_genesis_confirmed(&self.peer_ip)
            .await
        {
            return false;
        }
        if !context.peer_registry.has_whitelist().await {
            return false;
        }
        !context.peer_registry.is_whitelisted(&self.peer_ip).await
    }

    /// Trim a block list so that it serializes within the wire frame limit.
    /// When blocks contain many transactions, even 50 blocks can exceed 8MB.
    /// We binary-search for the largest prefix that fits.
    pub(super) fn trim_blocks_to_frame_limit(mut blocks: Vec<Block>) -> Vec<Block> {
        if blocks.is_empty() {
            return blocks;
        }
        // Quick check: does the full batch fit?
        let msg = NetworkMessage::BlocksResponse(blocks.clone());
        let size = bincode::serialized_size(&msg).unwrap_or(u64::MAX);
        if size <= crate::network::wire::MAX_FRAME_SIZE as u64 {
            return blocks;
        }
        // Binary search for the largest count that fits
        let mut lo: usize = 1;
        let mut hi: usize = blocks.len();
        let mut best: usize = 0;
        while lo <= hi {
            let mid = (lo + hi) / 2;
            let test_msg = NetworkMessage::BlocksResponse(blocks[..mid].to_vec());
            let test_size = bincode::serialized_size(&test_msg).unwrap_or(u64::MAX);
            if test_size <= crate::network::wire::MAX_FRAME_SIZE as u64 {
                best = mid;
                lo = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            }
        }
        if best < blocks.len() {
            warn!(
                "📦 Trimmed block response from {} to {} blocks to fit frame limit",
                blocks.len(),
                best
            );
            blocks.truncate(best);
        }
        blocks
    }

    /// Get voter weight from masternode registry, defaulting to 1 if not found
    pub(super) async fn get_voter_weight(registry: &MasternodeRegistry, voter_id: &str) -> u64 {
        match registry.get(voter_id).await {
            Some(info) => info.masternode.tier.sampling_weight().max(1),
            None => 1u64,
        }
    }

    /// Verify a vote signature (PREPARE or PRECOMMIT)
    /// Returns Ok(true) if valid, Ok(false) if invalid/rejected.
    ///
    /// Security: records violations against the sending peer for AV27 (forged
    /// signatures) and AV28 (unregistered voter spam), so the banlist escalation
    /// ladder can ban repeat offenders.
    pub(super) async fn verify_vote_signature(
        &self,
        registry: &MasternodeRegistry,
        block_hash: &[u8; 32],
        voter_id: &str,
        vote_type: &[u8], // b"PREPARE" or b"PRECOMMIT"
        signature: &[u8],
        context: &MessageContext,
    ) -> Result<bool, ()> {
        if signature.is_empty() {
            warn!(
                "❌ [{}] Rejecting unsigned {} vote from {} (signatures required)",
                self.direction,
                String::from_utf8_lossy(vote_type),
                voter_id
            );
            // AV27: unsigned vote is never legitimate; record immediately
            self.record_vote_violation(context, "unsigned vote (AV27)")
                .await;
            return Ok(false);
        }

        let Some(info) = registry.get(voter_id).await else {
            warn!(
                "❌ [{}] Rejecting {} vote from unknown/unregistered voter {}",
                self.direction,
                String::from_utf8_lossy(vote_type),
                voter_id
            );
            // AV28: rate-limit — votes may be legitimately relayed for recently-
            // deregistered nodes; only penalise after 10 rejections in 60 s.
            self.record_unregistered_vote(context).await;
            return Ok(false);
        };

        use ed25519_dalek::{Signature, Verifier};

        // Reconstruct the signed message
        let mut msg = Vec::new();
        msg.extend_from_slice(block_hash);
        msg.extend_from_slice(voter_id.as_bytes());
        msg.extend_from_slice(vote_type);

        // Parse signature
        let sig_array: [u8; 64] = match signature.try_into() {
            Ok(arr) => arr,
            Err(_) => {
                warn!(
                    "❌ [{}] Invalid {} signature length from {} (expected 64 bytes, got {})",
                    self.direction,
                    String::from_utf8_lossy(vote_type),
                    voter_id,
                    signature.len()
                );
                // AV27: wrong-length signature is always malformed; record immediately
                self.record_vote_violation(context, "invalid vote signature length (AV27)")
                    .await;
                return Ok(false); // Reject
            }
        };

        let sig = Signature::from_bytes(&sig_array);
        if let Err(e) = info.masternode.public_key.verify(&msg, &sig) {
            warn!(
                "❌ [{}] Invalid {} vote signature from {}: {}",
                self.direction,
                String::from_utf8_lossy(vote_type),
                voter_id,
                e
            );
            // AV27: use sliding window — a legitimate peer may have 1-2 stale
            // in-flight votes right after a block transition or key rotation.
            // Only record a violation after 5 failures within 30 s.
            self.record_invalid_sig_vote(context).await;
            return Ok(false); // Reject
        }

        Ok(true) // Valid signature
    }

    /// AV27: record a vote violation against the sending peer immediately.
    /// Used for structurally malformed votes (missing/wrong-length signature).
    /// Uses the banlist escalation ladder: 3 → 1-min ban, 5 → 5-min ban,
    /// 10 → permanent ban.
    pub(super) async fn record_vote_violation(&self, context: &MessageContext, reason: &str) {
        if let Some(banlist) = &context.banlist {
            if let Ok(ip) = self.peer_ip.parse::<IpAddr>() {
                banlist.write().await.record_violation(ip, reason);
            }
        }
    }

    /// Permanently ban this peer's IP address (strips port first).
    /// Whitelisted peers are never permanently banned — they are operator-trusted
    /// and a permanent ban would cut off Michigan from its reference peers.
    pub(super) async fn permanent_ban_ip(&self, context: &MessageContext, reason: &str) {
        if let Some(banlist) = &context.banlist {
            let bare = self.peer_ip.split(':').next().unwrap_or(&self.peer_ip);
            if let Ok(ip) = bare.parse::<IpAddr>() {
                if banlist.read().await.is_whitelisted(ip) {
                    warn!(
                        "⚠️ Suppressing permanent ban for whitelisted peer {} — reason: {}",
                        self.peer_ip, reason
                    );
                    return;
                }
                banlist.write().await.add_permanent_ban(ip, reason);
            }
        }
        self.suspend_peer_from_consensus(context, reason).await;
    }

    pub(super) async fn suspend_peer_from_consensus(&self, context: &MessageContext, reason: &str) {
        match context
            .masternode_registry
            .suspend_from_consensus(&self.peer_ip, reason)
            .await
        {
            Ok(()) => {}
            Err(crate::masternode_registry::RegistryError::NotFound) => {}
            Err(e) => {
                warn!(
                    "⚠️ [{}] Failed to suspend {} from consensus: {}",
                    self.direction, self.peer_ip, e
                );
            }
        }
    }

    pub(super) async fn clear_peer_consensus_suspension(&self, context: &MessageContext) {
        match context
            .masternode_registry
            .clear_consensus_suspension(&self.peer_ip)
            .await
        {
            Ok(()) => {}
            Err(crate::masternode_registry::RegistryError::NotFound) => {}
            Err(e) => {
                warn!(
                    "⚠️ [{}] Failed to clear consensus suspension for {}: {}",
                    self.direction, self.peer_ip, e
                );
            }
        }
    }

    /// AV27: sliding-window rate-limit for Ed25519 vote signature failures.
    /// Records one violation after 5 failures within 30 s, then resets.
    /// This prevents in-flight stale votes around block transitions (a legitimate
    /// peer sends at most 2 stale votes per block — one PREPARE, one PRECOMMIT)
    /// from triggering a ban.
    async fn record_invalid_sig_vote(&self, context: &MessageContext) {
        if check_sliding_window(&self.invalid_sig_vote_window, 5, 30).await {
            self.record_vote_violation(context, "invalid vote signature spam (AV27: 5+ per 30s)")
                .await;
            if let Some(ai) = &context.ai_system {
                ai.attack_detector
                    .record_invalid_vote_sig_spam(&self.peer_ip);
            }
        }
    }

    /// AV28: sliding-window rate-limit for unregistered-voter rejections.
    /// Records one violation after 10 rejections within a 60-second window,
    /// then resets the counter. Higher threshold than AV27 because a trusted
    /// relay peer may briefly forward votes from a recently-deregistered node.
    async fn record_unregistered_vote(&self, context: &MessageContext) {
        if check_sliding_window(&self.unregistered_vote_window, 10, 60).await {
            self.record_vote_violation(context, "unregistered voter spam (AV28: 10+ per 60s)")
                .await;
            if let Some(ai) = &context.ai_system {
                ai.attack_detector
                    .record_unregistered_voter_spam(&self.peer_ip);
            }
        }
    }

    /// Handle a network message and optionally return a response message
    ///
    /// # Arguments
    /// * `msg` - The message to handle
    /// * `context` - Shared context with blockchain, registries, etc.
    ///
    /// # Returns
    /// * `Ok(Some(response))` - Message handled successfully, send this response
    /// * `Ok(None)` - Message handled successfully, no response needed
    /// * `Err(msg)` - Error handling message
    pub async fn handle_message(
        &self,
        msg: &NetworkMessage,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // SECURITY: Check banlist before processing ANY message
        if let Some(banlist) = &context.banlist {
            if let Ok(ip) = self.peer_ip.parse::<IpAddr>() {
                let mut bl = banlist.write().await;
                if let Some(reason) = bl.is_banned(ip) {
                    warn!(
                        "🚫 [{:?}] REJECTING message from banned peer {}: {}",
                        self.direction, self.peer_ip, reason
                    );
                    return Err(format!("Peer {} is banned: {}", self.peer_ip, reason));
                }
            }
        }

        // SYNC GATE: During initial sync, only process messages essential for syncing.
        // This keeps the node laser-focused on catching up before doing anything else.
        if context.blockchain.is_syncing() {
            let is_sync_essential = matches!(
                msg,
                // Liveness
                NetworkMessage::Ping { .. }
                | NetworkMessage::Pong { .. }
                // Block sync (the actual sync work)
                | NetworkMessage::GetBlocks(_, _)
                | NetworkMessage::BlocksResponse(_)
                | NetworkMessage::BlockRangeResponse(_)
                | NetworkMessage::BlockResponse(_)
                | NetworkMessage::BlockRequest(_)
                | NetworkMessage::BlockAnnouncement(_)
                | NetworkMessage::BlockInventory(_)
                | NetworkMessage::GetBlockHeight
                | NetworkMessage::BlockHeightResponse(_)
                | NetworkMessage::GetBlockRange { .. }
                | NetworkMessage::GetBlockHash(_)
                | NetworkMessage::BlockHashResponse { .. }
                // Chain tip discovery
                | NetworkMessage::GetChainTip
                | NetworkMessage::ChainTipResponse { .. }
                // Genesis verification
                | NetworkMessage::GetGenesisHash
                | NetworkMessage::GenesisHashResponse(_)
                | NetworkMessage::RequestGenesis
                | NetworkMessage::GenesisAnnouncement(_)
                // Peer discovery (need peers to sync from)
                | NetworkMessage::GetPeers
                | NetworkMessage::PeersResponse(_)
                | NetworkMessage::PeerExchange(_)
                // Fork alerts (need to know if we're on wrong chain)
                | NetworkMessage::ForkAlert { .. }
            );

            if !is_sync_essential {
                debug!(
                    "⏸️ [{}] Deferring {} from {} (syncing)",
                    self.direction,
                    msg.message_type(),
                    self.peer_ip
                );
                return Ok(None);
            }
        }

        let result = match msg {
            // === Health Check Messages ===
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => self.handle_ping(*nonce, *timestamp, *height, context).await,
            NetworkMessage::Pong {
                nonce,
                timestamp,
                height,
            } => self.handle_pong(*nonce, *timestamp, *height, context).await,

            // === Block Sync Messages ===
            NetworkMessage::GetBlocks(start, end) => {
                self.handle_get_blocks(*start, *end, context).await
            }
            NetworkMessage::GetBlockHeight => self.handle_get_block_height(context).await,
            NetworkMessage::GetChainTip => self.handle_get_chain_tip(context).await,
            NetworkMessage::GetBlockRange {
                start_height,
                end_height,
            } => {
                self.handle_get_block_range(*start_height, *end_height, context)
                    .await
            }
            NetworkMessage::GetBlockHash(height) => {
                self.handle_get_block_hash(*height, context).await
            }
            NetworkMessage::BlockRequest(height) => {
                self.handle_block_request(*height, context).await
            }
            NetworkMessage::BlockInventory(height) => {
                self.handle_block_inventory(*height, context).await
            }
            NetworkMessage::BlockResponse(block) => {
                self.handle_block_response(block.clone(), context).await
            }
            NetworkMessage::BlockAnnouncement(block) => {
                self.handle_block_response(block.clone(), context).await
            }

            // === Genesis Messages ===
            NetworkMessage::GetGenesisHash => self.handle_get_genesis_hash(context).await,
            NetworkMessage::GenesisHashResponse(hash) => {
                self.handle_genesis_hash_response(*hash, context).await
            }
            NetworkMessage::RequestGenesis => self.handle_request_genesis(context).await,
            NetworkMessage::GenesisAnnouncement(block) => {
                self.handle_genesis_announcement(block.clone(), context)
                    .await
            }

            // === Transaction Messages ===
            NetworkMessage::TransactionBroadcast(tx) => {
                self.handle_transaction_broadcast(tx.clone(), context).await
            }
            NetworkMessage::TransactionFinalized { txid, tx } => {
                self.handle_transaction_finalized(*txid, tx.clone(), context)
                    .await
            }
            NetworkMessage::MempoolSyncRequest => self.handle_mempool_sync_request(context).await,
            NetworkMessage::MempoolSyncResponse(entries) => {
                self.handle_mempool_sync_response(entries.clone(), context)
                    .await
            }

            // === Peer Exchange Messages ===
            NetworkMessage::GetPeers => self.handle_get_peers(context).await,
            NetworkMessage::PeersResponse(peers) => {
                self.handle_peers_response(peers.clone(), context).await
            }
            NetworkMessage::PeerExchange(entries) => {
                self.handle_peer_exchange(entries.clone(), context).await
            }

            // === Masternode Messages ===
            NetworkMessage::GetMasternodes => self.handle_get_masternodes(context).await,
            NetworkMessage::MasternodeAnnouncement { .. } => {
                // V1 deprecated — all nodes use V2 now
                debug!(
                    "⏭️  [{}] Ignoring deprecated V1 masternode announcement from {}",
                    self.direction, self.peer_ip
                );
                Ok(None)
            }
            NetworkMessage::MasternodeAnnouncementV2 {
                address,
                reward_address,
                tier,
                public_key,
                collateral_outpoint,
            } => {
                // V2 without certificate — treat as empty certificate, no proof
                self.handle_masternode_announcement(
                    address.clone(),
                    reward_address.clone(),
                    *tier,
                    *public_key,
                    collateral_outpoint.clone(),
                    vec![0u8; 64],
                    0, // V2 has no started_at
                    vec![],
                    context,
                )
                .await
            }
            NetworkMessage::MasternodeAnnouncementV3 {
                address,
                reward_address,
                tier,
                public_key,
                collateral_outpoint,
                certificate,
                started_at,
            } => {
                self.handle_masternode_announcement(
                    address.clone(),
                    reward_address.clone(),
                    *tier,
                    *public_key,
                    collateral_outpoint.clone(),
                    certificate.clone(),
                    *started_at,
                    vec![],
                    context,
                )
                .await
            }
            NetworkMessage::MasternodeAnnouncementV4 {
                address,
                reward_address,
                tier,
                public_key,
                collateral_outpoint,
                certificate,
                started_at,
                collateral_proof,
            } => {
                self.handle_masternode_announcement(
                    address.clone(),
                    reward_address.clone(),
                    *tier,
                    *public_key,
                    collateral_outpoint.clone(),
                    certificate.clone(),
                    *started_at,
                    collateral_proof.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::MasternodeUnlock {
                address,
                collateral_outpoint,
                timestamp,
                signature,
            } => {
                self.handle_masternode_unlock(
                    address.clone(),
                    collateral_outpoint.clone(),
                    *timestamp,
                    signature.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::MasternodesResponse(masternodes) => {
                self.handle_masternodes_response(masternodes.clone(), context)
                    .await
            }
            NetworkMessage::MasternodeInactive { address, timestamp } => {
                self.handle_masternode_inactive(address.clone(), *timestamp, context)
                    .await
            }
            NetworkMessage::GetLockedCollaterals => {
                self.handle_get_locked_collaterals(context).await
            }
            NetworkMessage::LockedCollateralsResponse(collaterals) => {
                self.handle_locked_collaterals_response(collaterals.clone(), context)
                    .await
            }

            // === UTXO Messages ===
            NetworkMessage::UTXOStateQuery(outpoints) => {
                self.handle_utxo_state_query(outpoints.clone(), context)
                    .await
            }
            NetworkMessage::UTXOStateUpdate { outpoint, state } => {
                self.handle_utxo_state_update(outpoint.clone(), state.clone(), context)
                    .await
            }
            NetworkMessage::GetUTXOStateHash => self.handle_get_utxo_state_hash(context).await,
            NetworkMessage::GetUTXOSet => self.handle_get_utxo_set(context).await,

            // === Consensus Query Messages ===
            NetworkMessage::ConsensusQuery { height, block_hash } => {
                self.handle_consensus_query(*height, *block_hash, context)
                    .await
            }
            NetworkMessage::GetChainWork => self.handle_get_chain_work(context).await,
            NetworkMessage::GetChainWorkAt(height) => {
                self.handle_get_chain_work_at(*height, context).await
            }

            // === TimeLock Consensus Messages ===
            NetworkMessage::TimeLockBlockProposal { block } => {
                self.handle_timelock_block_proposal(block.clone(), context)
                    .await
            }
            NetworkMessage::TimeVotePrepare {
                block_hash,
                voter_id,
                signature,
            } => {
                self.handle_timelock_prepare_vote(
                    *block_hash,
                    voter_id.clone(),
                    signature.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::TimeVotePrecommit {
                block_hash,
                voter_id,
                signature,
            } => {
                self.handle_timelock_precommit_vote(
                    *block_hash,
                    voter_id.clone(),
                    signature.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::FinalityVoteBroadcast { vote } => {
                self.handle_finality_vote_broadcast(vote.clone(), context)
                    .await
            }

            // === §7.6 Liveness Fallback Protocol Messages ===
            NetworkMessage::LivenessAlert { alert } => {
                self.handle_liveness_alert(alert.clone(), context).await
            }
            NetworkMessage::FinalityProposal { proposal } => {
                self.handle_finality_proposal(proposal.clone(), context)
                    .await
            }
            NetworkMessage::FallbackVote { vote } => {
                self.handle_fallback_vote(vote.clone(), context).await
            }

            // === TimeVote Consensus Messages (§7 Transaction Finality) ===
            NetworkMessage::TimeVoteRequest {
                txid,
                tx_hash_commitment,
                slot_index,
                tx,
            } => {
                self.handle_timevote_request(
                    *txid,
                    *tx_hash_commitment,
                    *slot_index,
                    tx.clone(),
                    context,
                )
                .await
            }
            NetworkMessage::TimeVoteResponse { vote } => {
                self.handle_timevote_response(vote.clone(), context).await
            }
            NetworkMessage::TimeProofBroadcast { proof } => {
                self.handle_timeproof_broadcast(proof.clone(), context)
                    .await
            }

            // === Gossip-based Status Tracking ===
            NetworkMessage::MasternodeStatusGossip {
                reporter,
                visible_masternodes,
                timestamp,
            } => {
                tracing::debug!(
                    "📥 [{:?}] Processing gossip from {}: {} masternodes visible",
                    self.direction,
                    reporter,
                    visible_masternodes.len()
                );
                context
                    .masternode_registry
                    .process_status_gossip(
                        reporter.clone(),
                        visible_masternodes.clone(),
                        *timestamp,
                    )
                    .await;
                Ok(None)
            }

            NetworkMessage::MasternodeStartedAtGossip { entries } => {
                for (addr, started_at) in entries {
                    context
                        .masternode_registry
                        .update_daemon_started_at(addr, *started_at)
                        .await;
                }
                Ok(None)
            }

            // === Fork Alert ===
            NetworkMessage::ForkAlert {
                your_height,
                your_hash,
                consensus_height,
                consensus_hash,
                consensus_peer_count,
                message,
            } => {
                self.handle_fork_alert(
                    *your_height,
                    *your_hash,
                    *consensus_height,
                    *consensus_hash,
                    *consensus_peer_count,
                    message.clone(),
                    context,
                )
                .await
            }

            // === Chain Synchronization Response Messages ===
            NetworkMessage::ChainTipResponse { height, hash } => {
                self.handle_chain_tip_response(*height, *hash, context)
                    .await
            }
            NetworkMessage::BlocksResponse(blocks) | NetworkMessage::BlockRangeResponse(blocks) => {
                self.handle_blocks_response(blocks.clone(), context).await
            }

            // === UTXO Sync Response Messages ===
            NetworkMessage::UTXOStateHashResponse {
                hash,
                height,
                utxo_count,
            } => {
                self.handle_utxo_state_hash_response(*hash, *height, *utxo_count, context)
                    .await
            }
            NetworkMessage::UTXOSetResponse(utxos) => {
                self.handle_utxo_set_response(utxos.clone(), context).await
            }

            // === Other Response Messages (handled by caller) ===
            NetworkMessage::BlockHeightResponse(_)
            | NetworkMessage::BlockHashResponse { .. }
            | NetworkMessage::ConsensusQueryResponse { .. }
            | NetworkMessage::ChainWorkResponse { .. }
            | NetworkMessage::ChainWorkAtResponse { .. }
            | NetworkMessage::PendingTransactionsResponse(_) => {
                // Response messages - no further action needed in handler
                Ok(None)
            }

            NetworkMessage::UTXOStateResponse(states) => {
                self.handle_utxo_state_response(states.clone(), context)
                    .await
            }

            // === Payment Request Relay ===
            NetworkMessage::PaymentRequestRelay(request) => {
                // Validate signature before storing
                let pubkey_bytes = hex::decode(&request.pubkey_hex).unwrap_or_default();
                let sig_bytes = hex::decode(&request.signature_hex).unwrap_or_default();
                if pubkey_bytes.len() == 32 && sig_bytes.len() == 64 {
                    let mut pubkey = [0u8; 32];
                    pubkey.copy_from_slice(&pubkey_bytes);
                    let mut sig = [0u8; 64];
                    sig.copy_from_slice(&sig_bytes);
                    if let Ok(verifying_key) = ed25519_dalek::VerifyingKey::from_bytes(&pubkey) {
                        let ed_signature = ed25519_dalek::Signature::from_bytes(&sig);
                        let mut sign_data = Vec::new();
                        sign_data.extend_from_slice(request.id.as_bytes());
                        sign_data.extend_from_slice(request.from_address.as_bytes());
                        sign_data.extend_from_slice(request.to_address.as_bytes());
                        sign_data.extend_from_slice(&request.amount.to_le_bytes());
                        sign_data.extend_from_slice(request.memo.as_bytes());
                        sign_data.extend_from_slice(&request.timestamp.to_le_bytes());
                        if verifying_key
                            .verify_strict(&sign_data, &ed_signature)
                            .is_ok()
                        {
                            if let Some(ref consensus) = context.consensus {
                                // Cache requester pubkey
                                if let Some(ref um) = context.utxo_manager {
                                    um.register_pubkey(&request.from_address, pubkey);
                                }
                                let stored = consensus.store_payment_request(request.clone());
                                if stored {
                                    tracing::info!(
                                        "📬 Stored payment request {} from {} to {}",
                                        &request.id[..std::cmp::min(16, request.id.len())],
                                        request.from_address,
                                        request.to_address,
                                    );
                                    // Push WS notification to payer if subscribed
                                    if let Some(ref tx_sender) = context.tx_event_sender {
                                        let _ = tx_sender.send(
                                            crate::rpc::websocket::TransactionEvent {
                                                txid: format!("pr:{}", request.id),
                                                outputs: vec![
                                                    crate::rpc::websocket::TxOutputInfo {
                                                        address: request
                                                            .to_address
                                                            .clone(),
                                                        amount: request.amount as f64
                                                            / 100_000_000.0,
                                                        index: 0,
                                                    },
                                                ],
                                                timestamp: request.timestamp,
                                                status:
                                                    crate::rpc::websocket::TxEventStatus::PaymentRequest {
                                                        from_address: request
                                                            .from_address
                                                            .clone(),
                                                        memo: request.memo.clone(),
                                                        requester_name: request
                                                            .requester_name
                                                            .clone(),
                                                        pubkey_hex: request
                                                            .pubkey_hex
                                                            .clone(),
                                                        expires: request.expires,
                                                    },
                                            },
                                        );
                                    }
                                }
                            }
                        } else {
                            tracing::warn!(
                                "⚠️ Rejected payment request with invalid signature from {}",
                                self.peer_ip
                            );
                        }
                    }
                }
                Ok(None)
            }

            NetworkMessage::PaymentRequestResponse {
                ref id,
                ref requester_address,
                ref payer_address,
                accepted,
                ref txid,
            } => {
                // Remove the resolved request from local storage
                if let Some(ref consensus) = context.consensus {
                    consensus.remove_payment_request(id);
                }
                // Push WS notification to the requester if subscribed on this node
                if let Some(ref tx_sender) = context.tx_event_sender {
                    let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                        txid: format!("pr-resp:{}", id),
                        outputs: vec![crate::rpc::websocket::TxOutputInfo {
                            address: requester_address.clone(),
                            amount: 0.0,
                            index: 0,
                        }],
                        timestamp: chrono::Utc::now().timestamp(),
                        status: crate::rpc::websocket::TxEventStatus::PaymentRequestResponse {
                            request_id: id.clone(),
                            payer_address: payer_address.clone(),
                            accepted: *accepted,
                            txid: txid.clone(),
                        },
                    });
                }
                Ok(None)
            }

            NetworkMessage::PaymentRequestCancelled {
                ref id,
                ref requester_address,
            } => {
                // Retrieve payer before removing so we can notify them
                let payer_address = context
                    .consensus
                    .as_ref()
                    .and_then(|c| c.get_payment_request_payer(id))
                    .unwrap_or_default();

                if let Some(ref consensus) = context.consensus {
                    consensus.remove_payment_request(id);
                }
                if !payer_address.is_empty() {
                    if let Some(ref tx_sender) = context.tx_event_sender {
                        let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                            txid: format!("pr-cancel:{}", id),
                            outputs: vec![crate::rpc::websocket::TxOutputInfo {
                                address: payer_address,
                                amount: 0.0,
                                index: 0,
                            }],
                            timestamp: chrono::Utc::now().timestamp(),
                            status: crate::rpc::websocket::TxEventStatus::PaymentRequestCancelled {
                                request_id: id.clone(),
                                requester_address: requester_address.clone(),
                            },
                        });
                    }
                }
                Ok(None)
            }

            NetworkMessage::PaymentRequestViewed {
                ref id,
                ref requester_address,
                ref payer_address,
            } => {
                if let Some(ref tx_sender) = context.tx_event_sender {
                    let _ = tx_sender.send(crate::rpc::websocket::TransactionEvent {
                        txid: format!("pr-view:{}", id),
                        outputs: vec![crate::rpc::websocket::TxOutputInfo {
                            address: requester_address.clone(),
                            amount: 0.0,
                            index: 0,
                        }],
                        timestamp: chrono::Utc::now().timestamp(),
                        status: crate::rpc::websocket::TxEventStatus::PaymentRequestViewed {
                            request_id: id.clone(),
                            payer_address: payer_address.clone(),
                        },
                    });
                }
                Ok(None)
            }

            // === Governance messages ===
            NetworkMessage::GovernanceProposal(proposal) => {
                let gov = match context.blockchain.governance() {
                    Some(g) => g.clone(),
                    None => return Ok(None),
                };
                let treasury = context.blockchain.get_treasury_balance();
                match gov
                    .submit_proposal(proposal.clone(), &context.masternode_registry, treasury)
                    .await
                {
                    Ok(()) => {
                        tracing::info!(
                            "🏛️  [{}] Governance proposal {} accepted, gossiping",
                            self.peer_ip,
                            hex::encode(&proposal.id[..6])
                        );
                        if let Some(ref tx) = context.broadcast_tx {
                            let _ = tx.send(NetworkMessage::GovernanceProposal(proposal.clone()));
                        }
                    }
                    Err(e) if e.contains("already") => {} // idempotent duplicate
                    Err(e) => tracing::warn!("🏛️  Governance proposal rejected: {e}"),
                }
                Ok(None)
            }

            NetworkMessage::GovernanceVote(vote) => {
                let gov = match context.blockchain.governance() {
                    Some(g) => g.clone(),
                    None => return Ok(None),
                };
                match gov
                    .record_vote(vote.clone(), &context.masternode_registry)
                    .await
                {
                    Ok(true) => {
                        tracing::info!(
                            "🏛️  [{}] Governance vote recorded for {}, gossiping",
                            self.peer_ip,
                            hex::encode(&vote.proposal_id[..6])
                        );
                        if let Some(ref tx) = context.broadcast_tx {
                            let _ = tx.send(NetworkMessage::GovernanceVote(vote.clone()));
                        }
                    }
                    Ok(false) => {} // duplicate
                    Err(e) => tracing::warn!("🏛️  Governance vote rejected: {e}"),
                }
                Ok(None)
            }

            NetworkMessage::GetGovernanceState => {
                if let Some(gov) = context.blockchain.governance() {
                    let proposals = gov.list_proposals().await;
                    let mut all_votes = Vec::new();
                    for p in &proposals {
                        all_votes.extend(gov.get_votes_for(&p.id).await);
                    }
                    return Ok(Some(NetworkMessage::GovernanceStateResponse {
                        proposals,
                        votes: all_votes,
                    }));
                }
                Ok(None)
            }

            NetworkMessage::GovernanceStateResponse { proposals, votes } => {
                if let Some(gov) = context.blockchain.governance() {
                    let treasury = context.blockchain.get_treasury_balance();
                    for proposal in proposals {
                        let _ = gov
                            .submit_proposal(
                                proposal.clone(),
                                &context.masternode_registry,
                                treasury,
                            )
                            .await;
                    }
                    for vote in votes {
                        let _ = gov
                            .record_vote(vote.clone(), &context.masternode_registry)
                            .await;
                    }
                }
                Ok(None)
            }

            NetworkMessage::ConnectivityWarning { message } => {
                // A remote masternode has probed our P2P port and found it unreachable.
                // Log a prominent warning so the operator knows they need to fix this.
                warn!("🚨 CONNECTIVITY WARNING from {}: {}", self.peer_ip, message);
                warn!("🔌 Your node's P2P port is not publicly reachable from the internet.");
                warn!("   To earn block rewards you need a VPS or server with a static public IP");
                warn!("   and an open P2P port (mainnet: 24000, testnet: 24100).");
                warn!("   Home connections with NAT/firewall that block inbound connections are");
                warn!("   not eligible for rewards — only outbound-only nodes see this message.");
                // Also store as an operator message so the dashboard can display it
                if let Some(ref inbox) = context.operator_messages {
                    if let Ok(mut q) = inbox.lock() {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        q.push_front((
                            now,
                            self.peer_ip.clone(),
                            format!("Connectivity warning: {}", message),
                        ));
                        q.truncate(50);
                    }
                }
                Ok(None)
            }

            NetworkMessage::OperatorMessage {
                from,
                message,
                timestamp,
            } => {
                // Enforce a 500-character limit and strip control chars to prevent terminal injection.
                let safe_msg: String = message
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .take(500)
                    .collect();
                info!("📨 Operator message from {}: {}", from, safe_msg);
                if let Some(ref inbox) = context.operator_messages {
                    if let Ok(mut q) = inbox.lock() {
                        q.push_front((*timestamp, from.clone(), safe_msg));
                        q.truncate(50);
                    }
                }
                Ok(None)
            }

            // === UTXO reconciliation — lets out-of-sync nodes resync their UTXO state ===
            NetworkMessage::RequestUtxoReconciliation {
                at_height,
                block_hash,
            } => {
                // Verify the requested block is on our chain before serving the snapshot.
                let our_hash = context
                    .blockchain
                    .get_block_hash_at_height(*at_height)
                    .await;
                if our_hash != Some(*block_hash) {
                    debug!(
                        "[{}] RequestUtxoReconciliation from {} — block hash mismatch at height {}, ignoring",
                        self.direction, self.peer_ip, at_height
                    );
                    return Ok(None);
                }
                let utxos = context.blockchain.utxo_manager.list_all_utxos().await;
                let utxo_count = utxos.len();
                let height = *at_height;
                let chunks = split_utxos_into_chunks(utxos);
                let total = chunks.len() as u32;

                info!(
                    "[{}] Serving UTXO reconciliation snapshot ({} UTXOs → {} chunk(s)) to {} for height {}",
                    self.direction, utxo_count, total, self.peer_ip, height
                );

                if total == 1 {
                    return Ok(Some(NetworkMessage::UtxoReconciliationResponse {
                        at_height: height,
                        utxos: chunks.into_iter().next().unwrap_or_default(),
                    }));
                }

                // Multi-chunk: stream all but the last chunk directly.
                for (i, chunk) in chunks.iter().enumerate().take((total - 1) as usize) {
                    let msg = NetworkMessage::UtxoReconciliationChunk {
                        at_height: height,
                        index: i as u32,
                        total,
                        utxos: chunk.clone(),
                    };
                    let _ = context.peer_registry.send_to_peer(&self.peer_ip, msg).await;
                    tokio::task::yield_now().await;
                }

                let last_chunk = chunks.into_iter().last().unwrap_or_default();
                Ok(Some(NetworkMessage::UtxoReconciliationChunk {
                    at_height: height,
                    index: total - 1,
                    total,
                    utxos: last_chunk,
                }))
            }

            NetworkMessage::UtxoReconciliationResponse { at_height, utxos } => {
                info!(
                    "[{}] Received UTXO reconciliation snapshot from {} — {} UTXOs at height {}. Applying…",
                    self.direction,
                    self.peer_ip,
                    utxos.len(),
                    at_height
                );
                let mut applied = 0usize;
                for utxo in utxos {
                    // Only add UTXOs we don't already have — don't overwrite local finalized state.
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

            NetworkMessage::UTXOSetChunk {
                index,
                total,
                utxos,
            } => {
                self.handle_utxo_set_chunk(*index, *total, utxos.clone(), context)
                    .await
            }

            NetworkMessage::UtxoReconciliationChunk {
                at_height,
                index,
                total,
                utxos,
            } => {
                self.handle_utxo_reconciliation_chunk(
                    *at_height,
                    *index,
                    *total,
                    utxos.clone(),
                    context,
                )
                .await
            }

            // === Secure Messaging (TIME-MSG v1) ===
            NetworkMessage::MsgSubmit { envelope } => {
                let tier = if let Some(ref addr) = context.node_masternode_address {
                    context
                        .masternode_registry
                        .get(addr)
                        .await
                        .map(|info| info.masternode.tier)
                        .unwrap_or(crate::types::MasternodeTier::Free)
                } else {
                    crate::types::MasternodeTier::Free
                };
                let key_ref = context.relay_signing_key.as_deref();
                if let Some(key) = key_ref {
                    crate::messaging::handlers::handle_msg_submit(
                        envelope,
                        context.relay_store.as_ref(),
                        tier,
                        key,
                    )
                    .await
                } else {
                    Ok(None)
                }
            }

            NetworkMessage::MsgFetchPending {
                recipient_addr_hash,
                since,
            } => {
                if let Some(key) = context.relay_signing_key.as_deref() {
                    crate::messaging::handlers::handle_msg_fetch_pending(
                        recipient_addr_hash,
                        *since,
                        context.relay_store.as_ref(),
                        key,
                    )
                    .await
                } else {
                    Ok(None)
                }
            }

            NetworkMessage::MsgReadAck {
                ack,
                recipient_pubkey,
            } => {
                crate::messaging::handlers::handle_msg_read_ack(
                    ack,
                    recipient_pubkey,
                    context.relay_store.as_ref(),
                )
                .await
            }

            NetworkMessage::MsgAckQuery { msg_id } => {
                crate::messaging::handlers::handle_msg_ack_query(
                    msg_id,
                    context.relay_store.as_ref(),
                )
                .await
            }

            NetworkMessage::MsgPubkeyQuery { address_hash } => {
                if let Some(ref utxo_mgr) = context.utxo_manager {
                    crate::messaging::handlers::handle_pubkey_query(
                        address_hash,
                        utxo_mgr,
                        context.contacts_book.as_ref(),
                    )
                    .await
                } else {
                    Ok(Some(NetworkMessage::MsgPubkeyResponse {
                        address_hash: *address_hash,
                        pubkey: None,
                    }))
                }
            }

            NetworkMessage::MsgRelayAck { ack } => {
                crate::messaging::handlers::handle_msg_relay_ack(ack, &context.peer_registry)
            }

            NetworkMessage::MsgEnvelopes {
                recipient_addr_hash,
                envelopes,
            } => crate::messaging::handlers::handle_msg_envelopes(
                recipient_addr_hash,
                envelopes,
                &context.peer_registry,
            ),

            NetworkMessage::MsgAckResponse {
                msg_id,
                ack,
                delivery,
            } => {
                // Store ack/delivery events in local relay store if available
                if let Some(ref store) = context.relay_store {
                    if let Some(ref ack_bytes) = ack {
                        if let Ok(read_ack) =
                            serde_cbor::from_slice::<crate::messaging::types::ReadAck>(ack_bytes)
                        {
                            let _ = store.store_ack(
                                &read_ack,
                                &read_ack.recipient_sig[..32].try_into().unwrap_or([0u8; 32]),
                            );
                        }
                    }
                    let _ = delivery; // delivery events are informational
                }
                let _ = msg_id;
                Ok(None)
            }

            NetworkMessage::MsgPubkeyResponse {
                address_hash,
                pubkey,
            } => crate::messaging::handlers::handle_pubkey_response(
                address_hash,
                *pubkey,
                &context.peer_registry,
                context.contacts_book.as_ref(),
            ),

            NetworkMessage::MsgExpiryNotice { notice } => {
                if let Ok(n) =
                    serde_cbor::from_slice::<crate::messaging::types::ExpiryNotice>(notice)
                {
                    debug!(
                        "📨 Message {} expired before delivery",
                        hex::encode(n.msg_id)
                    );
                    if let Some(ref store) = context.relay_store {
                        let _ = store.set_status(
                            &n.msg_id,
                            &crate::messaging::types::MessageStatus::Expired,
                        );
                    }
                }
                Ok(None)
            }

            // === Messages not handled here ===
            _ => {
                debug!(
                    "[{}] Unhandled message type {:?} from {}",
                    self.direction,
                    msg.message_type(),
                    self.peer_ip
                );
                Ok(None)
            }
        };

        // Record AI events based on message processing results
        if let Some(ai) = &context.ai_system {
            // Record all messages as anomaly detector events (for traffic pattern analysis)
            ai.anomaly_detector
                .record_event(format!("msg_{}", msg.message_type()), 1.0);

            // Record errors as potential attack indicators
            if result.is_err() {
                ai.attack_detector.record_invalid_message(&self.peer_ip);
                ai.anomaly_detector
                    .record_event("invalid_message".to_string(), 1.0);
            }
        }

        result
    }
}
