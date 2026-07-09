use super::common::*;
use super::context::MessageContext;
use super::ConnectionDirection;
use super::MessageHandler;
use crate::network::message::NetworkMessage;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Fields shared by MasternodeAnnouncement V2/V3/V4 handlers.
/// Grouped to keep `handle_masternode_announcement` under Clippy's argument limit.
pub(super) struct MasternodeAnnouncementParams {
    pub announced_address: String,
    pub reward_address: String,
    pub tier: crate::types::MasternodeTier,
    pub public_key: ed25519_dalek::VerifyingKey,
    pub collateral_outpoint: Option<crate::types::OutPoint>,
    pub certificate: Vec<u8>,
    pub started_at: u64,
    pub collateral_proof: Vec<u8>,
}

impl MessageHandler {
    pub(super) async fn handle_get_masternodes(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetMasternodes request from {}",
            self.direction, self.peer_ip
        );

        let all_masternodes = context.masternode_registry.list_all().await;
        let mn_data: Vec<crate::network::message::MasternodeAnnouncementData> = all_masternodes
            .iter()
            .filter(|mn_info| {
                // Only relay masternodes that have been verified via a direct TCP
                // connection at least once (last_seen_at > 0), or are registered on-chain.
                // Gossip-only entries with last_seen_at == 0 have never successfully
                // connected to us — propagating them spreads stale/dead node addresses
                // across the network, causing every peer to repeatedly attempt and fail
                // connections to nodes that have been offline for a long time.
                let is_on_chain = matches!(
                    mn_info.registration_source,
                    crate::masternode_registry::RegistrationSource::OnChain(_)
                );
                if !is_on_chain && mn_info.last_seen_at == 0 {
                    return false;
                }
                // Never relay peers that have sent oversized frames — even whitelisted
                // friendly nodes running old code should not be propagated to the network
                // while they are frame-bombing. They will re-appear once they upgrade.
                if let Some(ai) = &context.ai_system {
                    let ip = mn_info
                        .masternode
                        .address
                        .split(':')
                        .next()
                        .unwrap_or(&mn_info.masternode.address);
                    if ai.attack_detector.is_known_frame_bomber(ip) {
                        debug!(
                            "Excluding known frame-bomber {} from MasternodesResponse",
                            ip
                        );
                        return false;
                    }
                }
                true
            })
            .map(|mn_info| {
                // Strip port from address to ensure consistency
                let ip_only = mn_info
                    .masternode
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&mn_info.masternode.address)
                    .to_string();
                crate::network::message::MasternodeAnnouncementData {
                    address: ip_only,
                    reward_address: mn_info.reward_address.clone(),
                    tier: mn_info.masternode.tier,
                    public_key: mn_info.masternode.public_key,
                    collateral_outpoint: mn_info.masternode.collateral_outpoint.clone(),
                    registered_at: mn_info.masternode.registered_at,
                }
            })
            .collect();

        debug!(
            "📤 [{}] Responded with {} masternode(s) to {}",
            self.direction,
            all_masternodes.len(),
            self.peer_ip
        );

        Ok(Some(NetworkMessage::MasternodesResponse(mn_data)))
    }

    /// Handle masternode inactive notification from network
    pub(super) async fn handle_masternode_inactive(
        &self,
        address: String,
        timestamp: u64,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📭 [{}] Received masternode inactive notification for {} from {}",
            self.direction, address, self.peer_ip
        );

        // Don't mark as inactive if we have a live connection to this node
        let ip_only = address.split(':').next().unwrap_or(&address);
        if context.peer_registry.is_connected(ip_only) {
            debug!(
                "⏭️ [{}] Ignoring inactive gossip for {} — we have a live connection",
                self.direction, address
            );
            return Ok(None);
        }

        match context
            .masternode_registry
            .mark_inactive_on_disconnect(&address)
            .await
        {
            Ok(()) => {
                debug!(
                    "✅ [{}] Marked masternode {} as inactive (timestamp: {})",
                    self.direction, address, timestamp
                );
            }
            // NotFound is expected: we may have already processed this disconnect ourselves
            // (our own TCP handler fires before peers relay the same notification).
            Err(crate::masternode_registry::RegistryError::NotFound) => {
                debug!(
                    "⏭️ [{}] Masternode {} already removed — duplicate inactive notification from {}",
                    self.direction, address, self.peer_ip
                );
            }
            Err(e) => {
                warn!(
                    "⚠️ [{}] Failed to mark masternode {} as inactive: {}",
                    self.direction, address, e
                );
            }
        }

        Ok(None)
    }

    /// Handle TimeLock Block Proposal - cache and vote
    pub(super) async fn handle_masternode_announcement(
        &self,
        params: MasternodeAnnouncementParams,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        let MasternodeAnnouncementParams {
            announced_address,
            reward_address,
            tier,
            public_key,
            collateral_outpoint,
            certificate,
            started_at,
            collateral_proof,
        } = params;

        let peer_ip = self.peer_ip.clone();
        // `announced_address` is the IP the masternode claims to operate on.
        // For direct connections this matches `peer_ip`; for relayed announcements
        // `peer_ip` is the relay node and `masternode_ip` is the actual masternode.
        let masternode_ip = announced_address.clone();
        // Detect relay: peer forwarded someone else's announcement
        let is_relayed = masternode_ip != peer_ip;

        debug!(
            "📨 [{}] Received masternode announcement from {} (tier: {:?}, masternode_ip: {}{})",
            self.direction,
            peer_ip,
            tier,
            masternode_ip,
            if is_relayed { " [relayed]" } else { "" }
        );

        // Certificate field ignored (certificate system removed in v1.2.0)
        let _ = &certificate;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Deferred-tier recovery: a node whose UTXO wasn't in local storage at startup
        // sets tier=Free provisionally but still includes its collateral outpoint.
        // Peers receiving this state reject it via AV40 (tier=Free with outpoint). Resolve
        // the contradiction here by looking up the UTXO and deriving the correct tier.
        // If found → upgrade to the real tier and proceed through the normal paid-tier path.
        // If not found or zero value → strip the outpoint so it registers cleanly as Free.
        let (tier, collateral_outpoint) =
            if tier == crate::types::MasternodeTier::Free && collateral_outpoint.is_some() {
                if let (Some(utxo_manager), Some(op)) =
                    (&context.utxo_manager, collateral_outpoint.as_ref())
                {
                    match utxo_manager.get_utxo(op).await {
                        Ok(utxo) if utxo.value > 0 => {
                            if let Some(derived) =
                                crate::types::MasternodeTier::from_collateral_value(utxo.value)
                            {
                                debug!(
                                    "📊 [{}] Deferred-tier upgrade: {} Free→{:?} via UTXO lookup",
                                    self.direction, masternode_ip, derived
                                );
                                (derived, collateral_outpoint)
                            } else {
                                // UTXO found but value doesn't match any tier — strip outpoint.
                                (tier, None)
                            }
                        }
                        Ok(_) => {
                            // UTXO found with zero value — strip outpoint (can't be collateral).
                            (tier, None)
                        }
                        Err(_) => {
                            // UTXO not found — may be unconfirmed collateral (deferred-tier state).
                            // Direct connection: keep outpoint so has_collateral=true on disconnect,
                            // preventing AV3 30s cooldown from cycling the connection.  AV40 in
                            // register_internal is relaxed for is_direct to allow this path through.
                            // Relayed: strip outpoint to block relay-based Free+outpoint pollution.
                            if !is_relayed {
                                (tier, collateral_outpoint)
                            } else {
                                (tier, None)
                            }
                        }
                    }
                } else {
                    // No UTXO manager — same direct/relay split as Err case above.
                    if !is_relayed {
                        (tier, collateral_outpoint)
                    } else {
                        (tier, None)
                    }
                }
            } else {
                (tier, collateral_outpoint)
            };

        if tier != crate::types::MasternodeTier::Free {
            // Staked tiers MUST include collateral_outpoint
            let outpoint = match collateral_outpoint {
                Some(op) => op,
                None => {
                    warn!(
                        "❌ [{}] Rejecting {:?} masternode from {} — no collateral outpoint",
                        self.direction, tier, peer_ip
                    );
                    return Ok(None);
                }
            };

            // ── Contested-outpoint fast-rejection (AV-banlist-exhaust) ─────────────────
            //
            // After ≥ CONTESTED_OUTPOINT_THRESHOLD unique IPs have been permanently banned
            // for claiming this outpoint without proof, skip all expensive UTXO / registry
            // work and immediately reject any new non-V4 claimant.  This prevents an
            // attacker from exhausting the ban list by spinning up cheap cloud VMs and
            // having each one force a full collateral check before being banned.
            //
            // V4 claimants (collateral_proof non-empty) always proceed normally so the
            // legitimate owner can still recover via cryptographic proof.
            let outpoint_key_str = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
            {
                let is_v4_claim = !collateral_proof.is_empty();
                let contested_count = contested_outpoints()
                    .get(&outpoint_key_str)
                    .map(|c| *c)
                    .unwrap_or(0);
                if contested_count >= CONTESTED_OUTPOINT_THRESHOLD && !is_v4_claim {
                    static CONTESTED_WARN: std::sync::OnceLock<
                        dashmap::DashMap<String, std::time::Instant>,
                    > = std::sync::OnceLock::new();
                    let wm = CONTESTED_WARN.get_or_init(dashmap::DashMap::new);
                    if should_warn_now(wm, &outpoint_key_str, 300) {
                        warn!(
                            "🛡️ [{}] Contested outpoint fast-reject: {} claims {} \
                             ({} prior bans, no V4 proof) — banning without UTXO work",
                            self.direction, masternode_ip, outpoint_key_str, contested_count
                        );
                    }
                    let bare = masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                    if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                        if let Some(bl) = &context.banlist {
                            if bl.write().await.is_banned(ban_ip).is_none() {
                                bl.write().await.add_permanent_ban(
                                    ban_ip,
                                    &format!(
                                        "Collateral hijack attempt for {} (contested outpoint, \
                                         {} prior bans)",
                                        outpoint_key_str, contested_count
                                    ),
                                );
                                *contested_outpoints()
                                    .entry(outpoint_key_str.clone())
                                    .or_insert(0) += 1;
                            }
                        }
                    }
                    if is_relayed {
                        return Ok(None);
                    }
                    return Err(format!(
                        "DISCONNECT: contested-outpoint squatter banned {}",
                        masternode_ip
                    ));
                }
            }

            // During initial sync (height far behind peers), skip collateral verification.
            // The UTXO set is incomplete/empty so verification would reject every staked
            // masternode, preventing us from syncing from the best peers.  Collateral will
            // be verified once we catch up.
            let our_height = context.blockchain.get_height();
            let still_syncing = our_height < 100;

            // Verify collateral UTXO on-chain (skip during initial sync)
            if !still_syncing {
                if let Some(utxo_manager) = &context.utxo_manager {
                    // Track whether the UTXO was found locally so the locking section below
                    // can skip the lock attempt when it's missing (avoids a NotFound rejection
                    // for nodes whose UTXO is genuinely on-chain but not yet in our local set).
                    let mut utxo_in_local_set = true;
                    match utxo_manager.get_utxo(&outpoint).await {
                        Ok(utxo) => {
                            let required = tier.collateral();
                            if utxo.value != required {
                                warn!(
                                "❌ [{}] Rejecting {:?} masternode from {} — collateral {} != required {}",
                                self.direction, tier, peer_ip, utxo.value, required
                            );
                                if utxo.value == 0 {
                                    if let Some(ai) = &context.ai_system {
                                        ai.attack_detector
                                            .record_zero_collateral_pollution(&peer_ip);
                                    }
                                }
                                return Ok(None);
                            }
                            // ── Collateral authorization hierarchy ──────────────────────────
                            //
                            // 1. utxo.masternode_key (strongest): the GUI wallet embedded the
                            //    authorized operator key when it created the collateral output.
                            //    No separate registration needed — the collateral tx IS the proof.
                            //
                            // 2. On-chain MasternodeReg operator_pubkey: explicit registration
                            //    tx for key rotation without spending the collateral again.
                            //
                            // 3. reward_address == utxo.address (Level 3 / legacy): for UTXOs
                            //    with no embedded key and no ProTx.  Strictly enforced — the
                            //    reward_address in the announcement MUST equal the collateral
                            //    UTXO's output address.  This prevents squatters from registering
                            //    with a victim's collateral outpoint under their OWN reward address.
                            //    Even if a squatter passes all other guards, their rewards are
                            //    forcibly directed to the UTXO owner's address (useless to them).
                            // ────────────────────────────────────────────────────────────────
                            let announcing_key_hex = hex::encode(public_key.as_bytes());

                            // Level 1: embedded masternode key in the collateral UTXO
                            if let Some(ref embedded_key_hex) = utxo.masternode_key {
                                if &announcing_key_hex != embedded_key_hex {
                                    static EMBED_WARN: std::sync::OnceLock<
                                        dashmap::DashMap<String, std::time::Instant>,
                                    > = std::sync::OnceLock::new();
                                    let wm = EMBED_WARN.get_or_init(dashmap::DashMap::new);
                                    if should_warn_now(wm, &peer_ip, 600) {
                                        warn!(
                                            "🚨 [{}] COLLATERAL KEY MISMATCH: {} claimed \
                                             collateral {} but node key {}…  != embedded key {}…",
                                            self.direction,
                                            peer_ip,
                                            outpoint,
                                            &announcing_key_hex[..16],
                                            &embedded_key_hex[..16]
                                        );
                                    }
                                    if let Some(ai) = &context.ai_system {
                                        ai.attack_detector.record_collateral_hijack(&peer_ip);
                                    }
                                    return Ok(None);
                                }
                                // Embedded key matches — skip further checks, allow through.
                                debug!(
                                    "✅ [{}] Collateral embedded key verified for {} {}",
                                    self.direction, peer_ip, outpoint
                                );
                            } else
                            // Level 2: on-chain MasternodeReg operator_pubkey
                            if let Some(registered_op_hex) = context
                                .masternode_registry
                                .get_operator_pubkey_for_collateral(&outpoint)
                                .await
                            {
                                if announcing_key_hex != registered_op_hex {
                                    static OP_WARN: std::sync::OnceLock<
                                        dashmap::DashMap<String, std::time::Instant>,
                                    > = std::sync::OnceLock::new();
                                    let wm = OP_WARN.get_or_init(dashmap::DashMap::new);
                                    if should_warn_now(wm, &peer_ip, 600) {
                                        warn!(
                                            "🚨 [{}] OPERATOR KEY MISMATCH: {} claimed collateral \
                                             {} but node key {} != registered operator {}",
                                            self.direction,
                                            peer_ip,
                                            outpoint,
                                            &announcing_key_hex[..16],
                                            &registered_op_hex[..16]
                                        );
                                    }
                                    if let Some(ai) = &context.ai_system {
                                        ai.attack_detector.record_collateral_hijack(&peer_ip);
                                    }
                                    return Ok(None);
                                }
                                // Keys match — this is the registered operator. Allow through
                                // without going through the reward_address collision check.
                                debug!(
                                    "✅ [{}] Operator key verified for {} collateral {}",
                                    self.direction, peer_ip, outpoint
                                );
                            } else {
                                // Level 3 fallback: no embedded key and no on-chain ProTx.
                                // The ONLY acceptable proof of ownership is that the reward
                                // address matches the UTXO output address.
                                //
                                // Enforcement: reject announcements where reward_address ≠
                                // utxo.address.  This ensures squatters — even those who
                                // get past other guards — can never redirect rewards to their
                                // own wallet.  The squatter would have to submit the real
                                // owner's address as their reward_address, gaining nothing.
                                //
                                // V4 proof (Level 1 key or Level 2 ProTx) is exempt from
                                // this check because cryptographic key ownership supersedes
                                // address matching.
                                if !utxo.address.is_empty() && reward_address != utxo.address {
                                    static REWARD_MISMATCH: std::sync::OnceLock<
                                        dashmap::DashMap<String, std::time::Instant>,
                                    > = std::sync::OnceLock::new();
                                    let wm = REWARD_MISMATCH.get_or_init(dashmap::DashMap::new);
                                    if should_warn_now(wm, &peer_ip, 300) {
                                        warn!(
                                            "🛡️ [{}] Rejecting {:?} masternode from {}: \
                                             reward_address {} does not match collateral \
                                             UTXO output address {} for {} \
                                             — obtain a V4 proof or use the correct \
                                             reward address",
                                            self.direction,
                                            tier,
                                            peer_ip,
                                            reward_address,
                                            utxo.address,
                                            outpoint
                                        );
                                    }
                                    if let Some(ai) = &context.ai_system {
                                        ai.attack_detector.record_collateral_spoof_attempt(
                                            &peer_ip,
                                            &outpoint.to_string(),
                                        );
                                    }
                                    return Ok(None);
                                }
                            }

                            // ── On-chain anchor check ───────────────────────────────────
                            //
                            // The `collateral_anchor:{outpoint}` sled key records the IP that
                            // produced the first confirmed on-chain MasternodeReg for this
                            // collateral.  Normally this is the legitimate owner — but a squatter
                            // who raced an on-chain registration first would poison the anchor.
                            //
                            // Resolution priority (highest wins):
                            //   1. Valid V4 proof — cryptographic signature by the private key
                            //      that controls the collateral UTXO output address.  This is
                            //      unforgeable and overrides any stale anchor.  Update the anchor
                            //      to the proving IP and evict the old squatter.
                            //   2. Anchor match — announcer IP matches the stored anchor with no
                            //      V4 proof.  Allow through normally.
                            //   3. Anchor mismatch, no V4 proof — ban the announcer as a squatter.
                            if let Some(anchored_ip) =
                                context.masternode_registry.get_collateral_anchor(&outpoint)
                            {
                                if anchored_ip != masternode_ip {
                                    // Check whether the announcer carries a valid V4 proof.
                                    // A V4 proof is a signature over "TIME_COLLATERAL_CLAIM:{txid}:{vout}"
                                    // by the private key of the collateral UTXO's output address.
                                    let has_valid_v4_proof =
                                        crate::address::verify_collateral_claim_proof(
                                            &public_key,
                                            &collateral_proof,
                                            &reward_address,
                                            &utxo.address,
                                            &outpoint.txid,
                                            outpoint.vout,
                                        );

                                    if has_valid_v4_proof {
                                        // Legitimate owner proved key ownership — the anchor was
                                        // poisoned by a squatter's earlier on-chain registration.
                                        // Evict the squatter, update the anchor, allow through.
                                        warn!(
                                            "🛡️ [{}] V4 proof overrides stale anchor for {}: \
                                             {} is the true owner (anchor was {}), evicting squatter",
                                            self.direction, outpoint, masternode_ip, anchored_ip
                                        );
                                        // Evict the squatter from registry and collateral lock.
                                        let _ = context
                                            .masternode_registry
                                            .unregister(&anchored_ip)
                                            .await;
                                        let _ = utxo_manager.unlock_collateral(&outpoint);
                                        // Update the anchor to the true owner.
                                        context
                                            .masternode_registry
                                            .set_collateral_anchor(&outpoint, &masternode_ip);
                                        // Ban the old anchored IP (the squatter).
                                        let bare_squatter =
                                            anchored_ip.split(':').next().unwrap_or(&anchored_ip);
                                        if let Ok(ban_ip) =
                                            bare_squatter.parse::<std::net::IpAddr>()
                                        {
                                            if let Some(bl) = &context.banlist {
                                                let mut guard = bl.write().await;
                                                guard.add_permanent_ban(
                                                    ban_ip,
                                                    "collateral squatter: evicted by V4 proof from true owner",
                                                );
                                            }
                                        }
                                        // Fall through — allow the true owner to register.
                                    } else {
                                        // No valid V4 proof and anchor disagrees — ban this announcer.
                                        static ANCHOR_BAN: std::sync::OnceLock<
                                            dashmap::DashMap<String, std::time::Instant>,
                                        > = std::sync::OnceLock::new();
                                        let wm = ANCHOR_BAN.get_or_init(dashmap::DashMap::new);
                                        if should_warn_now(wm, &masternode_ip, 300) {
                                            warn!(
                                                "🚨 [{}] ON-CHAIN ANCHOR VIOLATION: {} claims \
                                                 collateral {} but on-chain anchor belongs to {} \
                                                 — permanently banning squatter{}",
                                                self.direction,
                                                masternode_ip,
                                                outpoint,
                                                anchored_ip,
                                                if is_relayed {
                                                    format!(" (relayed via {})", peer_ip)
                                                } else {
                                                    String::new()
                                                }
                                            );
                                        }
                                        let bare = masternode_ip
                                            .split(':')
                                            .next()
                                            .unwrap_or(&masternode_ip);
                                        if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                                            if let Some(bl) = &context.banlist {
                                                let mut guard = bl.write().await;
                                                guard.add_permanent_ban(
                                                    ban_ip,
                                                    &format!("Collateral hijack attempt for {}:{} (on-chain anchor belongs to different IP)", hex::encode(outpoint.txid), outpoint.vout),
                                                );
                                                // Track this outpoint as contested so future
                                                // non-V4 claimants are rejected immediately.
                                                *contested_outpoints()
                                                    .entry(outpoint_key_str.clone())
                                                    .or_insert(0) += 1;
                                                if is_relayed {
                                                    let bare_relay = peer_ip
                                                        .split(':')
                                                        .next()
                                                        .unwrap_or(&peer_ip);
                                                    if let Ok(relay_ip) =
                                                        bare_relay.parse::<std::net::IpAddr>()
                                                    {
                                                        guard.record_violation(
                                                            relay_ip,
                                                            "relayed on-chain anchor squatter announcement",
                                                        );
                                                    }
                                                }
                                            }
                                            if let Some(ai) = &context.ai_system {
                                                ai.attack_detector.record_collateral_spoof_attempt(
                                                    &masternode_ip,
                                                    &outpoint.to_string(),
                                                );
                                            }
                                        }
                                        if context
                                            .masternode_registry
                                            .get_registered_ip_for_collateral(&outpoint)
                                            .await
                                            .as_deref()
                                            == Some(masternode_ip.as_str())
                                        {
                                            let _ = context
                                                .masternode_registry
                                                .unregister(&masternode_ip)
                                                .await;
                                            let _ = utxo_manager.unlock_collateral(&outpoint);
                                        }
                                        if is_relayed {
                                            return Ok(None);
                                        }
                                        return Err(format!(
                                            "DISCONNECT: on-chain anchor squatter banned {}",
                                            masternode_ip
                                        ));
                                    }
                                }
                            }
                            // ── End on-chain anchor check ────────────────────────────

                            if utxo_manager.is_collateral_locked(&outpoint) {
                                let existing = utxo_manager.get_locked_collateral(&outpoint);
                                if let Some(ref info) = existing {
                                    if info.masternode_address != masternode_ip {
                                        // Conflict: two different IPs claim the same collateral.
                                        //
                                        // Three-tier eviction priority (gossip only, no consensus impact):
                                        //
                                        //   Tier 1 — V4 proof: valid masternodeprivkey signature
                                        //     + reward_address == utxo.address.  Definitive proof of
                                        //     ownership; always evicts any gossip squatter.
                                        //
                                        //   Tier 2 — Address match: reward_address == utxo.address
                                        //     but no signature.  The claimant's rewards go to the UTXO
                                        //     owner's wallet — a squatter using their OWN reward_address
                                        //     gains nothing from contesting this.  Safe to evict any
                                        //     squatter whose reward_address != utxo.address.  If the
                                        //     current holder ALSO has reward_address == utxo.address
                                        //     (address-match stalemate), require a signature to break
                                        //     the tie and reject the new claimant.
                                        //
                                        //   Tier 3 — No match: reject (first-claim wins).
                                        let squatter_ip = info.masternode_address.clone();
                                        drop(existing);

                                        let claimant_matches_utxo = reward_address == utxo.address;

                                        // Look up the squatter's stored reward_address to detect
                                        // address-match stalemates.
                                        let squatter_reward_addr = context
                                            .masternode_registry
                                            .get_reward_address_for_ip(&squatter_ip)
                                            .await;
                                        let squatter_matches_utxo = squatter_reward_addr
                                            .as_deref()
                                            .map(|a| a == utxo.address)
                                            .unwrap_or(false);

                                        let has_valid_proof =
                                            crate::address::verify_collateral_claim_proof(
                                                &public_key,
                                                &collateral_proof,
                                                &reward_address,
                                                &utxo.address,
                                                &outpoint.txid,
                                                outpoint.vout,
                                            );

                                        let can_evict = if has_valid_proof {
                                            // Tier 1: cryptographic proof — but never evict the
                                            // local node via gossip regardless of proof strength;
                                            // the legitimate owner must file an on-chain
                                            // MasternodeReg tx to reclaim the collateral.
                                            let is_local_squatter = context
                                                .node_masternode_address
                                                .as_deref()
                                                .map(|local| local == squatter_ip)
                                                .unwrap_or(false);
                                            if is_local_squatter {
                                                static LOCAL_V4_WARN: std::sync::OnceLock<
                                                    dashmap::DashMap<String, std::time::Instant>,
                                                > = std::sync::OnceLock::new();
                                                let wm = LOCAL_V4_WARN
                                                    .get_or_init(dashmap::DashMap::new);
                                                if should_warn_now(wm, &masternode_ip, 120) {
                                                    warn!(
                                                        "🚨 [{}] COLLATERAL HIJACK BLOCKED: {} \
                                                         tried V4 eviction of local node {} \
                                                         for {} — banning attacker{}",
                                                        self.direction,
                                                        masternode_ip,
                                                        squatter_ip,
                                                        outpoint,
                                                        if is_relayed {
                                                            format!(" (via relay {})", peer_ip)
                                                        } else {
                                                            String::new()
                                                        }
                                                    );
                                                }
                                                // Immediately record banlist violation against
                                                // the actual attacker (masternode_ip), not the relay.
                                                if let Some(bl) = &context.banlist {
                                                    let bare = masternode_ip
                                                        .split(':')
                                                        .next()
                                                        .unwrap_or(&masternode_ip);
                                                    if let Ok(ban_ip) =
                                                        bare.parse::<std::net::IpAddr>()
                                                    {
                                                        let mut guard = bl.write().await;
                                                        guard.record_violation(
                                                            ban_ip,
                                                            "V4 collateral hijack attempt",
                                                        );
                                                        guard.record_violation(
                                                            ban_ip,
                                                            "V4 collateral hijack attempt",
                                                        );
                                                        guard.record_violation(
                                                            ban_ip,
                                                            "V4 collateral hijack attempt",
                                                        );
                                                    }
                                                }
                                                if let Some(ai) = &context.ai_system {
                                                    ai.attack_detector
                                                        .record_collateral_spoof_attempt(
                                                            &masternode_ip,
                                                            &outpoint.to_string(),
                                                        );
                                                }
                                                false
                                            } else {
                                                // Storm protection: rate-limit V4 evictions per
                                                // outpoint to break infinite cycling when multiple
                                                // nodes simultaneously hold valid V4 proofs.
                                                let outpoint_key = outpoint.to_string();
                                                let within_cooldown = v4_eviction_cooldown()
                                                    .get(&outpoint_key)
                                                    .map(|t| {
                                                        t.elapsed().as_secs()
                                                            < V4_EVICTION_COOLDOWN_SECS
                                                    })
                                                    .unwrap_or(false);
                                                if within_cooldown {
                                                    static STORM_WARN: std::sync::OnceLock<
                                                        dashmap::DashMap<
                                                            String,
                                                            std::time::Instant,
                                                        >,
                                                    > = std::sync::OnceLock::new();
                                                    let wm = STORM_WARN
                                                        .get_or_init(dashmap::DashMap::new);
                                                    if should_warn_now(wm, &outpoint_key, 30) {
                                                        warn!(
                                                            "🛡️ [{}] V4 eviction storm blocked \
                                                             for {} ({} → {}) — cooldown active",
                                                            self.direction,
                                                            outpoint,
                                                            squatter_ip,
                                                            peer_ip
                                                        );
                                                    }
                                                    if let Some(ai) = &context.ai_system {
                                                        ai.attack_detector
                                                            .record_eviction_storm_attempt(
                                                                &peer_ip,
                                                                &outpoint.to_string(),
                                                            );
                                                    }
                                                    false
                                                } else {
                                                    true
                                                }
                                            }
                                        } else if claimant_matches_utxo && !squatter_matches_utxo {
                                            // Tier 2: address-match beats address-mismatch squatter,
                                            // but ONLY for Free-tier squatters.
                                            //
                                            // SAFETY 1: never evict the local node via Tier 2 — a
                                            // remote peer that knows the UTXO's on-chain address could
                                            // spoof reward_address to match it and displace us.
                                            //
                                            // SAFETY 2: never evict any paid-tier squatter via Tier 2.
                                            // The UTXO output address is publicly visible on-chain; any
                                            // node can copy it into reward_address.  Additionally, when
                                            // a paid-tier node changes collateral (e.g. Bronze → Silver),
                                            // the old outpoint briefly stays in the UTXOManager with a
                                            // mismatched reward_address — Tier 2 must not steal it.
                                            // Only V4 cryptographic proof can displace a paid-tier node.
                                            let is_local_squatter = context
                                                .node_masternode_address
                                                .as_deref()
                                                .map(|local| local == squatter_ip)
                                                .unwrap_or(false);
                                            if is_local_squatter {
                                                warn!(
                                                    "🛡️ [{}] Blocked Tier 2 eviction attack: \
                                                     {} tried to displace local node {} for {} \
                                                     — cryptographic proof (V4) required",
                                                    self.direction, peer_ip, squatter_ip, outpoint
                                                );
                                                false
                                            } else {
                                                let squatter_tier = context
                                                    .masternode_registry
                                                    .get(&squatter_ip)
                                                    .await
                                                    .map(|info| info.masternode.tier)
                                                    .unwrap_or(crate::types::MasternodeTier::Free);
                                                if squatter_tier
                                                    != crate::types::MasternodeTier::Free
                                                {
                                                    warn!(
                                                        "🛡️ [{}] Blocked Tier 2 eviction of \
                                                         paid-tier squatter {}: {} tried to \
                                                         claim {} via reward_address match — \
                                                         V4 proof required",
                                                        self.direction,
                                                        squatter_ip,
                                                        peer_ip,
                                                        outpoint
                                                    );
                                                    false
                                                } else {
                                                    info!(
                                                        "✅ [{}] Address-match eviction: {} has \
                                                         reward_address == utxo.address for {} — \
                                                         evicting Free-tier squatter {} \
                                                         (mismatched address)",
                                                        self.direction,
                                                        peer_ip,
                                                        outpoint,
                                                        squatter_ip
                                                    );
                                                    true
                                                }
                                            }
                                        } else {
                                            false
                                        };

                                        if can_evict {
                                            if has_valid_proof {
                                                v4_eviction_cooldown().insert(
                                                    outpoint.to_string(),
                                                    std::time::Instant::now(),
                                                );
                                                // Arm the post-eviction lockout so the squatter
                                                // cannot immediately re-squat via free-tier migration
                                                // (Attack Vector 14 — V4 eviction oscillation).
                                                let eviction_key = format!(
                                                    "{}:{}",
                                                    hex::encode(outpoint.txid),
                                                    outpoint.vout
                                                );
                                                context
                                                    .masternode_registry
                                                    .record_v4_eviction(&eviction_key);
                                                info!(
                                                    "✅ [{}] V4 collateral proof verified: evicting \
                                                     squatter {} and registering legitimate owner {} \
                                                     for {}",
                                                    self.direction,
                                                    squatter_ip,
                                                    masternode_ip,
                                                    outpoint
                                                );
                                            }
                                            let _ = utxo_manager.unlock_collateral(&outpoint);
                                            let _ = context
                                                .masternode_registry
                                                .unregister(&squatter_ip)
                                                .await;
                                            // Permanently ban the evicted squatter so they cannot
                                            // immediately re-register.  V4 proof = definitive ban;
                                            // Tier 2 address-match = 3 violations (auto-temp-ban).
                                            let bare_squatter = squatter_ip
                                                .split(':')
                                                .next()
                                                .unwrap_or(&squatter_ip);
                                            if let Ok(ban_ip) =
                                                bare_squatter.parse::<std::net::IpAddr>()
                                            {
                                                if let Some(bl) = &context.banlist {
                                                    let mut guard = bl.write().await;
                                                    if has_valid_proof {
                                                        guard.add_permanent_ban(
                                                            ban_ip,
                                                            "collateral squatter evicted by V4 proof",
                                                        );
                                                        warn!(
                                                            "🔨 [{}] Permanently banned squatter {} \
                                                             (V4 proof confirmed ownership for {})",
                                                            self.direction, ban_ip, outpoint
                                                        );
                                                    } else {
                                                        // Tier 2 eviction: 3 violations → temp ban
                                                        for _ in 0..3 {
                                                            guard.record_violation(ban_ip, "collateral squatter (address mismatch)");
                                                        }
                                                    }
                                                }
                                            }
                                            // Fall through to lock and register the legitimate owner
                                        } else {
                                            // Gossip conflicts: reject the new claimant.
                                            static CONFLICT_WARN_TIMES: std::sync::OnceLock<
                                                dashmap::DashMap<String, std::time::Instant>,
                                            > = std::sync::OnceLock::new();
                                            let warn_map = CONFLICT_WARN_TIMES
                                                .get_or_init(dashmap::DashMap::new);
                                            if should_warn_now(warn_map, &masternode_ip, 600) {
                                                warn!(
                                                    "🚨 [{}] Collateral conflict: {} claimed {} \
                                                     already held by {} — gossip cannot prove \
                                                     ownership, use on-chain MasternodeReg{}",
                                                    self.direction,
                                                    masternode_ip,
                                                    outpoint,
                                                    squatter_ip,
                                                    if is_relayed {
                                                        format!(" (relayed via {})", peer_ip)
                                                    } else {
                                                        String::new()
                                                    }
                                                );
                                            }
                                            return Ok(None);
                                        }
                                    }
                                }
                            }
                            debug!(
                                "✅ [{}] Collateral verified for {:?} masternode {} ({} TIME)",
                                self.direction,
                                tier,
                                masternode_ip,
                                utxo.value as f64 / 100_000_000.0
                            );
                        }
                        Err(_) => {
                            utxo_in_local_set = false;
                            // UTXO not found in local set.  This can happen when:
                            //   (a) the collateral tx is unconfirmed (not yet in a block we've processed)
                            //   (b) the UTXO reindex hasn't run yet / is incomplete
                            //   (c) the node announced a UTXO that genuinely doesn't exist
                            //
                            // For direct connections we allow through rather than silently rejecting,
                            // since a UTXO miss is most likely transient (cases a/b).  Squatters using
                            // a fake outpoint lose nothing by being let through — their reward address
                            // won't match the actual UTXO owner once the UTXO appears, and they'll be
                            // evicted at that point.  For relayed announcements we still reject to
                            // limit unauthenticated gossip spread.
                            static UTXO_MISS_WARN: std::sync::OnceLock<
                                dashmap::DashMap<String, std::time::Instant>,
                            > = std::sync::OnceLock::new();
                            let wm = UTXO_MISS_WARN.get_or_init(dashmap::DashMap::new);
                            if is_relayed {
                                if should_warn_now(wm, &masternode_ip, 300) {
                                    warn!(
                                        "⚠️ [{}] Skipping relayed {:?} masternode {} — collateral UTXO not yet in local set",
                                        self.direction, tier, masternode_ip
                                    );
                                }
                                return Ok(None);
                            } else {
                                // Direct connection — allow through, skip collateral lock below.
                                // The node will be re-verified on next announcement once the UTXO
                                // appears in our set.
                                if should_warn_now(wm, &masternode_ip, 300) {
                                    warn!(
                                        "⚠️ [{}] Allowing direct {:?} masternode {} — collateral UTXO not yet in local set (will re-verify on next announcement)",
                                        self.direction, tier, masternode_ip
                                    );
                                }
                                // Skip the collateral lock — fall through to registration.
                            }
                        }
                    }

                    // Lock the collateral
                    //
                    // Before locking, also check the registry's in-memory nodes map for a
                    // conflicting claim.  The UTXOManager lock can be lost after a restart
                    // while the gossip registry entry survives — in that case
                    // `is_collateral_locked` above returns false and the V4 eviction block
                    // is bypassed.  This second check ensures the registry is always clean
                    // before we attempt to register the new node.
                    if let Some(registry_squatter) = if utxo_in_local_set {
                        context
                            .masternode_registry
                            .get_registered_ip_for_collateral(&outpoint)
                            .await
                    } else {
                        None
                    } {
                        if registry_squatter != masternode_ip {
                            // Re-fetch UTXO for address comparison.
                            let utxo_addr_opt = utxo_manager
                                .get_utxo(&outpoint)
                                .await
                                .ok()
                                .map(|u| u.address);

                            let claimant_matches_utxo = utxo_addr_opt
                                .as_deref()
                                .map(|a| a == reward_address)
                                .unwrap_or(false);

                            let squatter_reward_addr = context
                                .masternode_registry
                                .get_reward_address_for_ip(&registry_squatter)
                                .await;
                            let squatter_matches_utxo = squatter_reward_addr
                                .as_deref()
                                .and_then(|a| utxo_addr_opt.as_deref().map(|u| a == u))
                                .unwrap_or(false);

                            let has_valid_proof = match utxo_addr_opt.as_deref() {
                                Some(addr) => crate::address::verify_collateral_claim_proof(
                                    &public_key,
                                    &collateral_proof,
                                    &reward_address,
                                    addr,
                                    &outpoint.txid,
                                    outpoint.vout,
                                ),
                                None => false,
                            };

                            let can_evict = if has_valid_proof {
                                // Tier 1: cryptographic proof — but never evict the local node
                                // via gossip regardless of proof strength.
                                let is_local_squatter = context
                                    .node_masternode_address
                                    .as_deref()
                                    .map(|local| local == registry_squatter.as_str())
                                    .unwrap_or(false);
                                if is_local_squatter {
                                    static LOCAL_V4_WARN2: std::sync::OnceLock<
                                        dashmap::DashMap<String, std::time::Instant>,
                                    > = std::sync::OnceLock::new();
                                    let wm = LOCAL_V4_WARN2.get_or_init(dashmap::DashMap::new);
                                    if should_warn_now(wm, &masternode_ip, 120) {
                                        warn!(
                                            "🚨 [{}] COLLATERAL HIJACK BLOCKED: {} tried V4 \
                                             eviction of local node {} for {} (registry path) \
                                             — banning attacker{}",
                                            self.direction,
                                            masternode_ip,
                                            registry_squatter,
                                            outpoint,
                                            if is_relayed {
                                                format!(" (via relay {})", peer_ip)
                                            } else {
                                                String::new()
                                            }
                                        );
                                    }
                                    // Record violation against the actual attacker (masternode_ip).
                                    if let Some(bl) = &context.banlist {
                                        let bare = masternode_ip
                                            .split(':')
                                            .next()
                                            .unwrap_or(&masternode_ip);
                                        if let Ok(ban_ip) = bare.parse::<std::net::IpAddr>() {
                                            let mut guard = bl.write().await;
                                            guard.record_violation(
                                                ban_ip,
                                                "V4 collateral hijack attempt",
                                            );
                                            guard.record_violation(
                                                ban_ip,
                                                "V4 collateral hijack attempt",
                                            );
                                            guard.record_violation(
                                                ban_ip,
                                                "V4 collateral hijack attempt",
                                            );
                                        }
                                    }
                                    if let Some(ai) = &context.ai_system {
                                        ai.attack_detector.record_collateral_spoof_attempt(
                                            &masternode_ip,
                                            &outpoint.to_string(),
                                        );
                                    }
                                    false
                                } else {
                                    // Storm protection: rate-limit V4 evictions per outpoint
                                    let outpoint_key = outpoint.to_string();
                                    let within_cooldown = v4_eviction_cooldown()
                                        .get(&outpoint_key)
                                        .map(|t| t.elapsed().as_secs() < V4_EVICTION_COOLDOWN_SECS)
                                        .unwrap_or(false);
                                    if within_cooldown {
                                        static STORM_WARN2: std::sync::OnceLock<
                                            dashmap::DashMap<String, std::time::Instant>,
                                        > = std::sync::OnceLock::new();
                                        let wm = STORM_WARN2.get_or_init(dashmap::DashMap::new);
                                        if should_warn_now(wm, &outpoint_key, 30) {
                                            warn!(
                                                "🛡️ [{}] V4 eviction storm blocked for {} \
                                                 ({} → {}) — cooldown active (registry path)",
                                                self.direction,
                                                outpoint,
                                                registry_squatter,
                                                masternode_ip
                                            );
                                        }
                                        if let Some(ai) = &context.ai_system {
                                            ai.attack_detector.record_eviction_storm_attempt(
                                                &masternode_ip,
                                                &outpoint.to_string(),
                                            );
                                        }
                                        false
                                    } else {
                                        true
                                    }
                                }
                            } else if claimant_matches_utxo && !squatter_matches_utxo {
                                // SAFETY 1: never evict the local node via Tier 2 — see comment
                                // in the UTXOManager-locked path above.
                                // SAFETY 2: never evict a paid-tier squatter via Tier 2 — same
                                // rationale: UTXO output address is public; only V4 proof can
                                // displace a paid-tier canonical holder.
                                let is_local_squatter = context
                                    .node_masternode_address
                                    .as_deref()
                                    .map(|local| local == registry_squatter.as_str())
                                    .unwrap_or(false);
                                if is_local_squatter {
                                    warn!(
                                        "🛡️ [{}] Blocked Tier 2 eviction attack (registry): \
                                         {} tried to displace local node {} for {} \
                                         — cryptographic proof (V4) required",
                                        self.direction, peer_ip, registry_squatter, outpoint
                                    );
                                    false
                                } else {
                                    let squatter_tier = context
                                        .masternode_registry
                                        .get(&registry_squatter)
                                        .await
                                        .map(|info| info.masternode.tier)
                                        .unwrap_or(crate::types::MasternodeTier::Free);
                                    if squatter_tier != crate::types::MasternodeTier::Free {
                                        warn!(
                                            "🛡️ [{}] Blocked Tier 2 eviction of paid-tier \
                                             squatter {} (registry): {} tried to claim {} via \
                                             reward_address match — V4 proof required",
                                            self.direction, registry_squatter, peer_ip, outpoint
                                        );
                                        false
                                    } else {
                                        info!(
                                            "✅ [{}] Address-match eviction (registry): {} has \
                                             reward_address == utxo.address for {} — evicting \
                                             Free-tier squatter {} \
                                             (UTXOManager lock absent, mismatched address)",
                                            self.direction,
                                            masternode_ip,
                                            outpoint,
                                            registry_squatter
                                        );
                                        true
                                    }
                                }
                            } else {
                                false
                            };

                            if can_evict {
                                if has_valid_proof {
                                    v4_eviction_cooldown()
                                        .insert(outpoint.to_string(), std::time::Instant::now());
                                    // Arm post-eviction lockout (AV14).
                                    let eviction_key =
                                        format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
                                    context
                                        .masternode_registry
                                        .record_v4_eviction(&eviction_key);
                                    info!(
                                        "✅ [{}] V4 proof evicts registry squatter {} for {} \
                                         (UTXOManager lock was absent — registry-only eviction)",
                                        self.direction, registry_squatter, outpoint
                                    );
                                }
                                let _ = context
                                    .masternode_registry
                                    .unregister(&registry_squatter)
                                    .await;
                                // Also release the UTXOManager lock so the wallet can
                                // spend the UTXO again if the squatter held the lock.
                                let _ = utxo_manager.unlock_collateral(&outpoint);
                                // Permanently ban the evicted registry squatter.
                                let bare_squatter = registry_squatter
                                    .split(':')
                                    .next()
                                    .unwrap_or(&registry_squatter);
                                if let Ok(ban_ip) = bare_squatter.parse::<std::net::IpAddr>() {
                                    if let Some(bl) = &context.banlist {
                                        let mut guard = bl.write().await;
                                        if has_valid_proof {
                                            guard.add_permanent_ban(
                                                ban_ip,
                                                "collateral squatter evicted by V4 proof (registry path)",
                                            );
                                            warn!(
                                                "🔨 [{}] Permanently banned registry squatter {} \
                                                 (V4 proof confirmed ownership for {})",
                                                self.direction, ban_ip, outpoint
                                            );
                                        } else {
                                            for _ in 0..3 {
                                                guard.record_violation(ban_ip, "collateral squatter (address mismatch, registry path)");
                                            }
                                        }
                                    }
                                }
                            } else {
                                let outpoint_key = outpoint.to_string();

                                // Rate-limit the WARN to once per 5 minutes per masternode_ip.
                                // Without this, a Sybil subnet floods 200+ identical lines/second.
                                static CONFLICT_WARN: std::sync::OnceLock<
                                    dashmap::DashMap<String, std::time::Instant>,
                                > = std::sync::OnceLock::new();
                                let wm = CONFLICT_WARN.get_or_init(dashmap::DashMap::new);
                                if should_warn_now(wm, &masternode_ip, 300) {
                                    warn!(
                                        "🚨 [{}] Registry conflict: {} already holds {} — \
                                         no valid V4 proof from {}, rejecting{}",
                                        self.direction,
                                        registry_squatter,
                                        outpoint_key,
                                        masternode_ip,
                                        if is_relayed {
                                            format!(" (relayed via {})", peer_ip)
                                        } else {
                                            String::new()
                                        }
                                    );
                                }

                                // Record a violation against the actual claimant (masternode_ip).
                                if let Some(banlist) = &context.banlist {
                                    let bare_ip =
                                        masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                                    if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                                        let mut bl = banlist.write().await;
                                        bl.record_violation(
                                            ban_ip,
                                            "Registry conflict: claimed collateral without proof",
                                        );
                                    }
                                }

                                // Coordinated Sybil detection: if ≥5 unique IPs from the same
                                // /24 subnet have claimed the same outpoint within 60 seconds,
                                // treat it as a coordinated attack and subnet-ban them all.
                                {
                                    // subnet_key = "w.x.y" (first 3 octets of masternode IPv4)
                                    let bare_ip =
                                        masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                                    let subnet_key = bare_ip
                                        .rsplit_once('.')
                                        .map(|x| x.0)
                                        .unwrap_or(bare_ip)
                                        .to_string();
                                    let tracker_key = format!("{}|{}", subnet_key, outpoint_key);

                                    // Static: outpoint+subnet → Vec<(ip, timestamp)>
                                    static SYBIL_TRACKER: std::sync::OnceLock<
                                        dashmap::DashMap<String, Vec<(String, std::time::Instant)>>,
                                    > = std::sync::OnceLock::new();
                                    let tracker = SYBIL_TRACKER.get_or_init(dashmap::DashMap::new);

                                    let mut entry = tracker.entry(tracker_key.clone()).or_default();
                                    let now = std::time::Instant::now();
                                    // Evict stale entries (>60s)
                                    entry.retain(|(_, ts)| ts.elapsed().as_secs() < 60);
                                    // Add this IP if not already present in the window
                                    if !entry.iter().any(|(ip, _)| ip == bare_ip) {
                                        entry.push((bare_ip.to_string(), now));
                                    }
                                    let unique_count = entry.len();
                                    drop(entry);

                                    if unique_count >= 5 {
                                        if let Some(banlist) = &context.banlist {
                                            let cidr = format!("{}.0/24", subnet_key);
                                            let mut bl = banlist.write().await;
                                            if bl.subnet_ban_count() < 256 {
                                                bl.add_subnet_ban(
                                                    &cidr,
                                                    &format!(
                                                        "Sybil attack: {} IPs from {} \
                                                         all claiming {} without proof",
                                                        unique_count, cidr, outpoint_key
                                                    ),
                                                );
                                                warn!(
                                                    "🚫 [AI] Auto-banned subnet {} — \
                                                     {} coordinated hijack attempts on {}",
                                                    cidr, unique_count, outpoint_key
                                                );
                                            }
                                        }
                                        // Clear the tracker for this key to avoid re-triggering
                                        tracker.remove(&tracker_key);
                                    }
                                }

                                return Ok(None);
                            }
                        }
                    }

                    if utxo_in_local_set {
                        let lock_height = context.blockchain.get_height();
                        if let Err(e) = utxo_manager.lock_collateral(
                            outpoint.clone(),
                            masternode_ip.clone(),
                            lock_height,
                            tier.collateral(),
                        ) {
                            if matches!(e, crate::utxo_manager::UtxoError::LockedAsCollateral) {
                                // Already locked (e.g., rebuilt on startup or peer reconnected) — this is fine
                                tracing::debug!(
                                    "🔒 [{}] Collateral for {} already locked — proceeding",
                                    self.direction,
                                    masternode_ip
                                );
                            } else {
                                warn!(
                                "❌ [{}] Rejecting {:?} masternode from {} — failed to lock collateral: {:?}",
                                self.direction, tier, masternode_ip, e
                            );
                                return Ok(None);
                            }
                        }
                    }
                } else {
                    warn!(
                        "⚠️ [{}] Cannot verify collateral for {} — no UTXO manager available",
                        self.direction, masternode_ip
                    );
                    return Ok(None);
                }
            } else {
                info!(
                    "⏳ [{}] Accepting {:?} masternode {} provisionally (height {} — syncing, collateral check deferred)",
                    self.direction, tier, masternode_ip, our_height
                );
            }

            // Create masternode with verified collateral.
            // Use masternode_ip (announced_address) as the masternode's identity — for relayed
            // announcements this is the actual masternode IP, not the relay's TCP source IP.
            let outpoint_for_relay = outpoint.clone();
            let mn = crate::types::Masternode::new_with_collateral(
                masternode_ip.clone(),
                reward_address.clone(),
                tier.collateral(),
                outpoint,
                public_key,
                tier,
                now,
            );

            // Ghost-registration guard: if the announced masternode IP is itself banned
            // (e.g., permanently banned for prior attacks), don't let it re-enter the registry
            // through a relay path.  Banned nodes can gossip through legitimate peers and
            // would otherwise bypass the per-connection banlist check at the TCP layer.
            if let Some(banlist) = &context.banlist {
                let bare_ip = masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                    if let Some(reason) = banlist.write().await.is_banned(ban_ip) {
                        tracing::debug!(
                            "🚫 [{}] Skipping gossip registration of banned masternode {} ({})",
                            self.direction,
                            masternode_ip,
                            reason
                        );
                        return Ok(None);
                    }
                }
            }

            let is_new = context
                .masternode_registry
                .get(&masternode_ip)
                .await
                .is_none();

            let reg_result = if !is_relayed {
                context
                    .masternode_registry
                    .register_direct(mn, reward_address.clone())
                    .await
            } else {
                // Gossip relay: store for peer discovery but do NOT activate.
                // Activation only happens on direct TCP handshake (register_direct).
                context
                    .masternode_registry
                    .register_gossip(mn, reward_address.clone())
                    .await
            };
            match reg_result {
                Ok(()) => {
                    // Collateral was verified on-chain above — mark as OnChain so the
                    // node is NOT removed as a "transient Free-tier" on disconnect.
                    let lock_h = context.blockchain.get_height();
                    let _ = context
                        .masternode_registry
                        .set_registration_source(
                            &masternode_ip,
                            crate::masternode_registry::RegistrationSource::OnChain(lock_h),
                        )
                        .await;

                    let count = context.masternode_registry.total_count().await;
                    debug!(
                        "✅ [{}] Registered {:?} masternode {} (total: {}{})",
                        self.direction,
                        tier,
                        masternode_ip,
                        count,
                        if is_relayed {
                            format!(", via relay {}", peer_ip)
                        } else {
                            String::new()
                        }
                    );
                    if let Some(peer_manager) = &context.peer_manager {
                        peer_manager.add_peer(masternode_ip.clone()).await;
                    }
                    if is_new {
                        if let Some(broadcast_tx) = &context.broadcast_tx {
                            let relay = if !collateral_proof.is_empty() {
                                crate::network::message::NetworkMessage::MasternodeAnnouncementV4 {
                                    address: masternode_ip.clone(),
                                    reward_address,
                                    tier,
                                    public_key,
                                    collateral_outpoint: Some(outpoint_for_relay),
                                    certificate: Vec::new(),
                                    started_at,
                                    collateral_proof: collateral_proof.clone(),
                                }
                            } else {
                                crate::network::message::NetworkMessage::MasternodeAnnouncementV3 {
                                    address: masternode_ip.clone(),
                                    reward_address,
                                    tier,
                                    public_key,
                                    collateral_outpoint: Some(outpoint_for_relay),
                                    certificate: Vec::new(),
                                    started_at,
                                }
                            };
                            let _ = broadcast_tx.send(relay);
                            debug!(
                                "📡 [{}] Relayed new {:?} masternode {} announcement to all peers",
                                self.direction, tier, masternode_ip
                            );
                        }
                    }
                    // Store remote daemon start time for uptime display
                    context
                        .masternode_registry
                        .update_daemon_started_at(&masternode_ip, started_at)
                        .await;
                }
                Err(crate::masternode_registry::RegistryError::CollateralAlreadyLocked) => {
                    // AV36 (relay poisoning): when the announce arrived via relay, the
                    // violation must be recorded against the RELAY peer (peer_ip), not
                    // the claimed masternode_ip. An attacker can craft an announce with
                    // masternode_ip = victim_ip pointing to already-locked collateral and
                    // relay it through any node — if we blamed masternode_ip here, the
                    // victim would accumulate severe violations on every receiving node.
                    let violation_ip = if is_relayed { &peer_ip } else { &masternode_ip };
                    warn!(
                        "❌ [{}] Collateral hijack attempt for {} — recording violation against {}{}",
                        self.direction,
                        outpoint_for_relay,
                        violation_ip,
                        if is_relayed {
                            format!(" (relay forwarded bad announce claiming {})", masternode_ip)
                        } else {
                            String::new()
                        }
                    );
                    if !is_relayed {
                        // Only flag the AI for direct squatting — relay is just forwarding
                        if let Some(ai) = &context.ai_system {
                            ai.attack_detector.record_collateral_spoof_attempt(
                                &masternode_ip,
                                &outpoint_for_relay.to_string(),
                            );
                        }
                    }
                    // CollateralAlreadyLocked is no longer treated as a severe ban-worthy
                    // offense.  An unforgeable V4 proof — verified upstream against the
                    // UTXO output address — is the canonical path for the legitimate owner
                    // to displace a stale anchor, and that path doesn't reach this branch.
                    // Reaching here means either (a) the announcer truly is a squatter, in
                    // which case the announcement is dropped harmlessly without rewards
                    // (see reward-address Level-3 enforcement), or (b) the local node has a
                    // stale anchor and the legitimate owner hasn't sent a V4 proof yet —
                    // banning them in that case is exactly the bug we're fixing.
                    //
                    // Rate-limit a soft violation per (peer, outpoint) so a determined
                    // squatter still accumulates banlist signal slowly; legitimate-owner
                    // reannounces every 60s no longer ratchet to a permaban within minutes.
                    if let Some(banlist) = &context.banlist {
                        let bare_ip = violation_ip.split(':').next().unwrap_or(violation_ip);
                        if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                            static LOCKED_VIOLATION_RL: std::sync::OnceLock<
                                dashmap::DashMap<String, std::time::Instant>,
                            > = std::sync::OnceLock::new();
                            let rl = LOCKED_VIOLATION_RL.get_or_init(dashmap::DashMap::new);
                            let key = format!("{}:{}", bare_ip, outpoint_for_relay);
                            if should_warn_now(rl, &key, 600) {
                                let mut bl = banlist.write().await;
                                bl.record_violation(
                                    ban_ip,
                                    &format!(
                                        "Collateral already locked under different anchor for {}{}",
                                        outpoint_for_relay,
                                        if is_relayed { " (relayed)" } else { "" }
                                    ),
                                );
                            }
                        }
                    }
                }
                Err(crate::masternode_registry::RegistryError::IpCyclingRejected) => {
                    if let Some(banlist) = &context.banlist {
                        let bare_ip = masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                        if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                            let mut bl = banlist.write().await;
                            let should_disconnect = bl.record_violation(ban_ip, "IP cycling (AV3)");
                            if should_disconnect && !is_relayed {
                                return Err(format!("DISCONNECT: IP cycling banned {}", ban_ip));
                            }
                        }
                    }
                }
                Err(crate::masternode_registry::RegistryError::CollateralRewardRedirect) => {
                    // SEVERE: node is claiming collateral whose on-chain UTXO owner address
                    // does not match the announced wallet/reward address. The old collateral
                    // is already gone, so this is not ambiguous churn — it is a deliberate
                    // attempt to route block rewards to an address that doesn't own the
                    // collateral. Issue an immediate disconnect and record a hard violation.
                    let violation_ip = if is_relayed { &peer_ip } else { &masternode_ip };
                    warn!(
                        "🚨 [{}] SEVERE — Reward-redirect attack: {} announced collateral {} \
                         but UTXO owner address does not match wallet/reward address — \
                         recording hard violation against {} and disconnecting{}",
                        self.direction,
                        masternode_ip,
                        outpoint_for_relay,
                        violation_ip,
                        if is_relayed {
                            format!(" (relayed by {})", peer_ip)
                        } else {
                            String::new()
                        }
                    );
                    if let Some(ai) = &context.ai_system {
                        ai.attack_detector.record_reward_redirect_attempt(
                            violation_ip,
                            &outpoint_for_relay.to_string(),
                        );
                    }
                    if let Some(banlist) = &context.banlist {
                        let bare_ip = violation_ip.split(':').next().unwrap_or(violation_ip);
                        if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                            // Not rate-limited — reward-redirect is always severe and deliberate.
                            let mut bl = banlist.write().await;
                            bl.record_violation(
                                ban_ip,
                                &format!(
                                    "Reward-redirect: claimed {} but UTXO owner != reward address",
                                    outpoint_for_relay
                                ),
                            );
                        }
                    }
                    if !is_relayed {
                        return Err(format!(
                            "DISCONNECT: Reward-redirect attack from {}",
                            masternode_ip
                        ));
                    }
                }
                Err(e) => {
                    warn!(
                        "❌ [{}] Failed to register masternode {}: {}",
                        self.direction, masternode_ip, e
                    );
                }
            }
        } else {
            // Free tier — no collateral verification needed.
            // For Free tier, announced_address == masternode_ip (relay detection still applies
            // but Free tier nodes are not authenticated so we use peer_ip as fallback).

            // Ghost-registration guard: banned nodes must not re-enter the registry
            // via gossip relay from legitimate peers.
            if let Some(banlist) = &context.banlist {
                let bare_ip = masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                    if let Some(reason) = banlist.write().await.is_banned(ban_ip) {
                        tracing::debug!(
                            "🚫 [{}] Skipping gossip registration of banned Free-tier node {} ({})",
                            self.direction,
                            masternode_ip,
                            reason
                        );
                        return Ok(None);
                    }
                }
            }

            let is_new = context
                .masternode_registry
                .get(&masternode_ip)
                .await
                .is_none();

            let mn = crate::types::Masternode::new_legacy(
                masternode_ip.clone(),
                reward_address.clone(),
                0,
                public_key,
                tier,
                now,
            );

            let reg_result = if !is_relayed {
                context
                    .masternode_registry
                    .register_direct(mn, reward_address.clone())
                    .await
            } else {
                // Gossip relay: store for peer discovery but do NOT activate.
                context
                    .masternode_registry
                    .register_gossip(mn, reward_address.clone())
                    .await
            };
            match reg_result {
                Ok(()) => {
                    let count = context.masternode_registry.total_count().await;
                    debug!(
                        "✅ [{}] Registered Free masternode {} (total: {})",
                        self.direction, masternode_ip, count
                    );
                    if let Some(peer_manager) = &context.peer_manager {
                        peer_manager.add_peer(masternode_ip.clone()).await;
                    }
                    // Relay to all other peers so nodes not directly connected to this
                    // masternode still learn about it (large-network discovery).
                    if is_new {
                        if let Some(broadcast_tx) = &context.broadcast_tx {
                            let relay =
                                crate::network::message::NetworkMessage::MasternodeAnnouncementV3 {
                                    address: masternode_ip.clone(),
                                    reward_address,
                                    tier,
                                    public_key,
                                    collateral_outpoint: None,
                                    certificate: Vec::new(),
                                    started_at,
                                };
                            let _ = broadcast_tx.send(relay);
                            debug!(
                                "📡 [{}] Relayed new Free masternode {} announcement to all peers",
                                self.direction, masternode_ip
                            );
                        }
                    }
                    // Store remote daemon start time for uptime display
                    context
                        .masternode_registry
                        .update_daemon_started_at(&masternode_ip, started_at)
                        .await;
                }
                Err(crate::masternode_registry::RegistryError::CollateralAlreadyLocked) => {
                    // AV36: same relay-poisoning guard as the paid-tier path above.
                    let violation_ip = if is_relayed { &peer_ip } else { &masternode_ip };
                    warn!(
                        "❌ [{}] Free-tier collateral hijack for {} — recording violation against {}{}",
                        self.direction,
                        masternode_ip,
                        violation_ip,
                        if is_relayed { " (relayed)" } else { "" }
                    );
                    if !is_relayed {
                        if let Some(ai) = &context.ai_system {
                            ai.attack_detector
                                .record_collateral_spoof_attempt(&masternode_ip, "free-tier-claim");
                        }
                    }
                    if let Some(banlist) = &context.banlist {
                        let bare_ip = violation_ip.split(':').next().unwrap_or(violation_ip);
                        if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                            let mut bl = banlist.write().await;
                            let should_disconnect = if is_relayed {
                                bl.record_violation(
                                    ban_ip,
                                    "Relayed free-tier collateral hijack announce",
                                )
                            } else {
                                bl.record_severe_violation(
                                    ban_ip,
                                    "Free-tier collateral hijack: tried to claim paid-tier collateral",
                                )
                            };
                            if should_disconnect && !is_relayed {
                                return Err(format!(
                                    "DISCONNECT: free-tier collateral hijack banned {}",
                                    ban_ip
                                ));
                            }
                        }
                    }
                }
                Err(crate::masternode_registry::RegistryError::IpCyclingRejected) => {
                    if let Some(banlist) = &context.banlist {
                        let bare_ip = masternode_ip.split(':').next().unwrap_or(&masternode_ip);
                        if let Ok(ban_ip) = bare_ip.parse::<std::net::IpAddr>() {
                            let mut bl = banlist.write().await;
                            let should_disconnect = bl.record_violation(ban_ip, "IP cycling (AV3)");
                            if should_disconnect && !is_relayed {
                                return Err(format!("DISCONNECT: IP cycling banned {}", ban_ip));
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "❌ [{}] Failed to register masternode {}: {}",
                        self.direction, masternode_ip, e
                    );
                }
            }
        }

        // === Reachability check ===
        // If this is an outbound connection (we dialed them), they are by definition
        // publicly reachable — mark immediately so they qualify for rewards.
        //
        // If this is an inbound connection (they connected to us), we must probe
        // their P2P port to verify they accept inbound connections too. Nodes that
        // only connect outbound (Windows/home users behind NAT) cannot serve the
        // network fully and are excluded from block rewards until the probe succeeds.
        let is_outbound = self.direction == ConnectionDirection::Outbound;
        if is_outbound {
            context
                .masternode_registry
                .set_publicly_reachable(&masternode_ip, true)
                .await;
        } else {
            // Spawn a background probe so we don't block message processing.
            // Rate-limited: try_claim_reachability_probe returns false if a probe
            // was already performed within REACHABILITY_RECHECK_SECS (10 min), so
            // we don't fire a new TCP probe on every 60-second announcement.
            // Use masternode_ip (not peer_ip) so relayed announcements probe the
            // actual masternode's port, not the relay's port.
            if context
                .masternode_registry
                .try_claim_reachability_probe(&masternode_ip)
                .await
            {
                let registry_clone = Arc::clone(&context.masternode_registry);
                let peer_registry_clone = Arc::clone(&context.peer_registry);
                let probe_addr = masternode_ip.clone();
                let network = context.masternode_registry.network();
                tokio::spawn(async move {
                    probe_masternode_reachability(
                        probe_addr,
                        network,
                        registry_clone,
                        peer_registry_clone,
                    )
                    .await;
                });
            }
        }

        Ok(None)
    }

    /// Handle MasternodeUnlock — signed collateral release gossip (analogous to Dash's ProUpRevTx).
    ///
    /// The message is accepted if either:
    ///   (a) `signature` is non-empty and verifies the revoke proof against the public key
    ///       stored in the registry for `address`, OR
    ///   (b) `signature` is empty AND the TCP source IP matches the masternode IP in `address`
    ///       (direct, non-relayed message from the node itself — legacy/first-boot compat).
    ///
    /// On acceptance:
    ///   1. The masternode is unregistered from the registry.
    ///   2. The collateral outpoint is queued for UTXO unlock.
    ///   3. The message is relayed to all other peers.
    pub(super) async fn handle_masternode_unlock(
        &self,
        address: String,
        collateral_outpoint: crate::types::OutPoint,
        timestamp: u64,
        signature: Vec<u8>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        // Reject stale timestamps (>10 min old or >5 min in future).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if timestamp + 600 < now || timestamp > now + 300 {
            warn!(
                "⚠️ [{}] Rejecting MasternodeUnlock for {} — stale timestamp (ts={}, now={})",
                self.direction, address, timestamp, now
            );
            return Ok(None);
        }

        // Determine the source IP for the direct-peer check.
        let peer_ip_only = self.peer_ip.split(':').next().unwrap_or(&self.peer_ip);
        let node_ip_only = address.split(':').next().unwrap_or(&address);

        let is_direct_from_node = peer_ip_only == node_ip_only;

        // Look up the existing registration for signature verification and to obtain the outpoint.
        let existing = context.masternode_registry.get(&address).await;

        let accepted = if !signature.is_empty() {
            // Verify the revoke proof signature:
            //   message = "TIME_COLLATERAL_REVOKE:<address>:<txid_hex>:<vout>:<timestamp>"
            // signed by the node's masternodeprivkey (stored as public_key in the registry).
            if let Some(ref info) = existing {
                let txid_hex = hex::encode(collateral_outpoint.txid);
                let proof_msg = format!(
                    "TIME_COLLATERAL_REVOKE:{}:{}:{}:{}",
                    address, txid_hex, collateral_outpoint.vout, timestamp
                );
                use ed25519_dalek::Verifier;
                let pub_key = &info.masternode.public_key;
                match ed25519_dalek::Signature::from_slice(&signature) {
                    Ok(sig) => {
                        if pub_key.verify(proof_msg.as_bytes(), &sig).is_ok() {
                            info!(
                                "🔓 [{}] MasternodeUnlock accepted for {} — valid signed revoke",
                                self.direction, address
                            );
                            true
                        } else {
                            warn!(
                                "⚠️ [{}] MasternodeUnlock rejected for {} — invalid signature from {}",
                                self.direction, address, self.peer_ip
                            );
                            return Ok(None);
                        }
                    }
                    Err(_) => {
                        warn!(
                            "⚠️ [{}] MasternodeUnlock rejected for {} — malformed signature from {}",
                            self.direction, address, self.peer_ip
                        );
                        return Ok(None);
                    }
                }
            } else {
                // Not in registry; can't verify.  Ignore to avoid relaying unverifiable revokes.
                debug!(
                    "⏭️ [{}] MasternodeUnlock for unknown node {} from {} — not in registry, ignoring",
                    self.direction, address, self.peer_ip
                );
                return Ok(None);
            }
        } else if is_direct_from_node {
            // Unsigned but from the node itself — accept as a legacy/first-boot revoke.
            info!(
                "🔓 [{}] MasternodeUnlock accepted for {} — unsigned, direct from node {}",
                self.direction, address, self.peer_ip
            );
            true
        } else {
            // Unsigned from a relay — cannot verify ownership.  Reject.
            warn!(
                "⚠️ [{}] MasternodeUnlock rejected for {} — unsigned relay from {} (not the node itself)",
                self.direction, address, self.peer_ip
            );
            return Ok(None);
        };

        if !accepted {
            return Ok(None);
        }

        // Unregister from registry (removes in-memory entry + sled anchor).
        match context.masternode_registry.unregister(&address).await {
            Ok(Some(removed)) => {
                // Queue the collateral UTXO for unlock so it becomes spendable again.
                if let Some(ref op) = removed.masternode.collateral_outpoint {
                    context
                        .masternode_registry
                        .queue_collateral_unlock(op.clone());
                    info!(
                        "🔓 Queued collateral {}:{} for unlock after MasternodeUnlock for {}",
                        hex::encode(op.txid),
                        op.vout,
                        address
                    );
                }
                // Also unlock the outpoint named in the message itself (may differ if the
                // registry had a different outpoint due to earlier upgrade).
                context
                    .masternode_registry
                    .queue_collateral_unlock(collateral_outpoint.clone());
            }
            Ok(None) | Err(crate::masternode_registry::RegistryError::NotFound) => {
                // Not in registry; still queue the outpoint from the message.
                context
                    .masternode_registry
                    .queue_collateral_unlock(collateral_outpoint.clone());
                debug!(
                    "⏭️ [{}] MasternodeUnlock for {} — node not in registry, queued outpoint unlock anyway",
                    self.direction, address
                );
            }
            Err(e) => {
                warn!(
                    "⚠️ [{}] MasternodeUnlock: failed to unregister {}: {}",
                    self.direction, address, e
                );
            }
        }

        // Relay to other peers.
        let relay_msg = NetworkMessage::MasternodeUnlock {
            address,
            collateral_outpoint,
            timestamp,
            signature,
        };
        context.peer_registry.broadcast(relay_msg).await;

        Ok(None)
    }

    /// Handle MasternodesResponse
    pub(super) async fn handle_masternodes_response(
        &self,
        masternodes: Vec<crate::network::message::MasternodeAnnouncementData>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received MasternodesResponse from {} with {} masternode(s)",
            self.direction,
            self.peer_ip,
            masternodes.len()
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // BOOTSTRAP MODE: At genesis (height 0), mark masternodes as active
        // This allows fresh nodes to discover each other and produce first blocks
        let current_height = context.blockchain.get_height();
        let is_bootstrap = current_height == 0;

        // Get local masternode address to skip self-overwrites from peer gossip
        let local_address = context.masternode_registry.get_local_address().await;

        let mut registered = 0;
        for mn_data in masternodes {
            // Don't let peer gossip overwrite our own masternode entry
            if let Some(ref local_addr) = local_address {
                let mn_ip = mn_data
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&mn_data.address);
                let local_ip = local_addr.split(':').next().unwrap_or(local_addr);
                if mn_ip == local_ip {
                    continue;
                }
            }

            // Deferred-tier nodes announce as Free + collateral_outpoint during
            // initial sync before their UTXO confirms.  Peer exchange passes this
            // state verbatim, which would trigger AV40 in register_internal.
            // Strip the outpoint here; the node will upgrade to its real tier via
            // a direct MasternodeAnnouncement once it connects to us.
            let effective_outpoint = if mn_data.tier == crate::types::MasternodeTier::Free {
                None
            } else {
                mn_data.collateral_outpoint.clone()
            };

            let masternode = if let Some(outpoint) = effective_outpoint {
                crate::types::Masternode::new_with_collateral(
                    mn_data.address.clone(),
                    mn_data.reward_address.clone(),
                    mn_data.tier.collateral(),
                    outpoint,
                    mn_data.public_key,
                    mn_data.tier,
                    now,
                )
            } else {
                crate::types::Masternode::new_legacy(
                    mn_data.address.clone(),
                    mn_data.reward_address.clone(),
                    mn_data.tier.collateral(),
                    mn_data.public_key,
                    mn_data.tier,
                    now,
                )
            };

            // BOOTSTRAP: Mark as active at genesis to allow block production
            // NORMAL: Register as inactive (will become active via direct P2P connection)
            let should_activate = is_bootstrap;

            match context
                .masternode_registry
                .register_internal(masternode, mn_data.reward_address, should_activate, false)
                .await
            {
                Ok(true) => registered += 1, // truly new masternode discovered
                Ok(false) => {}              // already known, no-op
                Err(_) => {}                 // rejected (cooldown, collateral conflict, etc.)
            }
        }

        if registered > 0 {
            if is_bootstrap {
                info!(
                    "✓ [{}] Bootstrap mode: Registered {} masternode(s) as ACTIVE from peer exchange",
                    self.direction, registered
                );
            } else {
                info!(
                    "✓ [{}] Discovered {} new masternode(s) from peer exchange — waking PHASE3",
                    self.direction, registered
                );
                // Wake PHASE3 only when genuinely new masternodes were discovered,
                // so it dials them instead of waiting up to 30s for the next scheduled tick.
                context
                    .masternode_registry
                    .priority_reconnect_notify()
                    .notify_one();
            }
        }

        Ok(None)
    }

    /// Handle GetLockedCollaterals request
    pub(super) async fn handle_get_locked_collaterals(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received GetLockedCollaterals request from {}",
            self.direction, self.peer_ip
        );

        // Get all locked collaterals from UTXO manager
        if let Some(utxo_manager) = &context.utxo_manager {
            let locked_collaterals = utxo_manager.list_locked_collaterals();

            let collateral_data: Vec<crate::network::message::LockedCollateralData> =
                locked_collaterals
                    .into_iter()
                    .map(|lc| crate::network::message::LockedCollateralData {
                        outpoint: lc.outpoint,
                        masternode_address: lc.masternode_address,
                        lock_height: lc.lock_height,
                        locked_at: lc.locked_at,
                        amount: lc.amount,
                    })
                    .collect();

            info!(
                "📤 [{}] Responded with {} locked collateral(s) to {}",
                self.direction,
                collateral_data.len(),
                self.peer_ip
            );

            Ok(Some(NetworkMessage::LockedCollateralsResponse(
                collateral_data,
            )))
        } else {
            // No UTXO manager available, return empty list
            Ok(Some(NetworkMessage::LockedCollateralsResponse(Vec::new())))
        }
    }

    /// Handle LockedCollateralsResponse
    pub(super) async fn handle_locked_collaterals_response(
        &self,
        collaterals: Vec<crate::network::message::LockedCollateralData>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        info!(
            "📥 [{}] Received LockedCollateralsResponse from {} with {} collateral(s)",
            self.direction,
            self.peer_ip,
            collaterals.len()
        );

        if let Some(utxo_manager) = &context.utxo_manager {
            let mut synced = 0;
            let mut conflicts = 0;
            let mut invalid = 0;

            for collateral_data in collaterals {
                // Verify the UTXO exists in our UTXO set
                match utxo_manager.get_utxo(&collateral_data.outpoint).await {
                    Ok(utxo) => {
                        // Verify amount matches
                        if utxo.value != collateral_data.amount {
                            warn!(
                                "⚠️ [{}] Collateral amount mismatch for {:?}: expected {}, got {}",
                                self.direction,
                                collateral_data.outpoint,
                                collateral_data.amount,
                                utxo.value
                            );
                            invalid += 1;
                            continue;
                        }

                        // Verify collateral ownership via canonical anchor.
                        // Only the first IP to register this outpoint (or the on-chain
                        // MasternodeReg signer) is authoritative. If a different peer claims
                        // the same outpoint, reject their lock — they cannot prove ownership
                        // via gossip alone.
                        let anchor_ip = context
                            .masternode_registry
                            .get_collateral_anchor(&collateral_data.outpoint);
                        if let Some(ref canonical) = anchor_ip {
                            let canonical_ip = canonical.split(':').next().unwrap_or(canonical);
                            let claiming_ip = collateral_data
                                .masternode_address
                                .split(':')
                                .next()
                                .unwrap_or(&collateral_data.masternode_address);
                            if canonical_ip != claiming_ip {
                                warn!(
                                    "🚨 [{}] Rejecting collateral lock from {} for {:?}: \
                                     outpoint is anchored to {}",
                                    self.direction,
                                    collateral_data.masternode_address,
                                    collateral_data.outpoint,
                                    canonical
                                );
                                invalid += 1;
                                continue;
                            }
                        }

                        // Check if already locked
                        if utxo_manager.is_collateral_locked(&collateral_data.outpoint) {
                            // Already locked - potential conflict or duplicate
                            let existing =
                                utxo_manager.get_locked_collateral(&collateral_data.outpoint);

                            if let Some(existing_lock) = existing {
                                if existing_lock.masternode_address
                                    != collateral_data.masternode_address
                                {
                                    warn!(
                                        "⚠️ [{}] Collateral conflict for {:?}: locked by {} (peer says {})",
                                        self.direction,
                                        collateral_data.outpoint,
                                        existing_lock.masternode_address,
                                        collateral_data.masternode_address
                                    );
                                    conflicts += 1;
                                }
                                // else: same lock, no action needed
                            }
                            continue;
                        }

                        // Lock the collateral
                        match utxo_manager.lock_collateral(
                            collateral_data.outpoint.clone(),
                            collateral_data.masternode_address.clone(),
                            collateral_data.lock_height,
                            collateral_data.amount,
                        ) {
                            Ok(()) => {
                                synced += 1;
                            }
                            Err(e) => {
                                warn!(
                                    "⚠️ [{}] Failed to lock collateral {:?}: {:?}",
                                    self.direction, collateral_data.outpoint, e
                                );
                                invalid += 1;
                            }
                        }
                    }
                    Err(_) => {
                        // UTXO doesn't exist in our set
                        warn!(
                            "⚠️ [{}] Collateral UTXO {:?} not found in our UTXO set",
                            self.direction, collateral_data.outpoint
                        );
                        invalid += 1;
                    }
                }
            }

            if synced > 0 {
                info!(
                    "✓ [{}] Synced {} locked collateral(s) from peer (conflicts: {}, invalid: {})",
                    self.direction, synced, conflicts, invalid
                );
            }
        }

        Ok(None)
    }
}
