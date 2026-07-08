use super::types::extract_ip;
use super::PeerConnectionRegistry;
use crate::network::message::NetworkMessage;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

impl PeerConnectionRegistry {
    /// Set banlist reference (called once after server initialization)
    pub async fn set_banlist(&self, banlist: Arc<RwLock<crate::network::banlist::IPBanlist>>) {
        *self.banlist.write().await = Some(banlist);
    }

    /// Check if a peer IP is whitelisted (trusted masternode from time-coin.io)
    pub async fn is_whitelisted(&self, peer_ip: &str) -> bool {
        let ip_only = extract_ip(peer_ip);
        if let Ok(ip_addr) = ip_only.parse::<IpAddr>() {
            if let Some(banlist) = self.banlist.read().await.as_ref() {
                return banlist.read().await.is_whitelisted(ip_addr);
            }
        }
        false
    }

    /// True when at least one IP is on the whitelist. Used by the IBD guard
    /// to skip the check entirely when neither time-coin.io nor any `addnode`
    /// entry yielded a valid IP — otherwise a brand-new node with no
    /// whitelist would be unable to sync at all.
    pub async fn has_whitelist(&self) -> bool {
        if let Some(banlist) = self.banlist.read().await.as_ref() {
            return banlist.read().await.whitelist_count() > 0;
        }
        false
    }

    /// Duration after which incompatible peers are re-checked (5 minutes)
    /// Note: Genesis mismatch is PERMANENT and never rechecked
    const INCOMPATIBLE_RECHECK_SECS: u64 = 300;
    /// TTL for the compatible peers cache (seconds)
    const COMPATIBLE_PEERS_CACHE_TTL: u64 = 10;

    /// Mark a peer as incompatible (different chain/hash calculation)
    /// If `permanent` is true (genesis mismatch), peer is never rechecked
    /// If `permanent` is false, peer is rechecked after INCOMPATIBLE_RECHECK_SECS
    pub async fn mark_incompatible(&self, peer_ip: &str, reason: &str, permanent: bool) {
        let ip_only = extract_ip(peer_ip).to_string();
        // Whitelisted peers are operator-trusted; never mark them incompatible.
        // Compatibility issues with whitelisted peers are local registry / version
        // drift, not a property of the peer.
        if self.is_whitelisted(&ip_only).await {
            tracing::warn!(
                "⚠️ Suppressing mark_incompatible for whitelisted peer {}: {} (permanent={})",
                ip_only,
                reason,
                permanent
            );
            return;
        }
        let mut incompatible = self.incompatible_peers.write().await;

        // Check if already marked
        if !incompatible.contains_key(&ip_only) {
            if permanent {
                // Genesis mismatch — genuinely wrong chain, log prominently
                tracing::error!(
                    "🚫 ═══════════════════════════════════════════════════════════════════"
                );
                tracing::error!("🚫 INCOMPATIBLE PEER DETECTED: {}", ip_only);
                tracing::error!("🚫 Reason: {}", reason);
                tracing::error!(
                    "🚫 This is a GENESIS MISMATCH — peer will be PERMANENTLY ignored."
                );
                tracing::error!("🚫 Peer should update software and resync from genesis.");
                tracing::error!(
                    "🚫 ═══════════════════════════════════════════════════════════════════"
                );
            } else {
                // Timeout / old-code — expected after our genesis-check-on-connect policy
                tracing::warn!(
                    "🚫 [{}] Marked temporarily incompatible: {} (re-check in {} min)",
                    ip_only,
                    reason,
                    Self::INCOMPATIBLE_RECHECK_SECS / 60
                );
            }
        }

        incompatible.insert(
            ip_only,
            (std::time::Instant::now(), reason.to_string(), permanent),
        );
    }

    /// Check if a peer is marked as incompatible (with automatic expiry for non-permanent)
    pub async fn is_incompatible(&self, peer_ip: &str) -> bool {
        let ip_only = extract_ip(peer_ip);
        let incompatible = self.incompatible_peers.read().await;

        if let Some((marked_at, _reason, permanent)) = incompatible.get(ip_only) {
            // Permanent incompatibility (genesis mismatch) - NEVER expires
            if *permanent {
                return true;
            }

            // Check if enough time has passed to re-check
            if marked_at.elapsed().as_secs() >= Self::INCOMPATIBLE_RECHECK_SECS {
                // Time to re-check - return false to allow retry
                drop(incompatible);
                // Clear the entry so they get a fresh chance
                self.incompatible_peers.write().await.remove(ip_only);
                tracing::info!(
                    "🔄 Re-checking previously incompatible peer {} ({}min timeout expired)",
                    ip_only,
                    Self::INCOMPATIBLE_RECHECK_SECS / 60
                );
                return false;
            }
            true
        } else {
            false
        }
    }

    /// Clear incompatible status for a peer (when they resync or update)
    pub async fn clear_incompatible(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip).to_string();
        if self
            .incompatible_peers
            .write()
            .await
            .remove(&ip_only)
            .is_some()
        {
            tracing::info!("✅ Peer {} is now compatible - blocks accepted", ip_only);
        }
    }

    /// Mark a peer as truly incompatible (different software/hashing algorithm)
    /// This should ONLY be called when genesis hash doesn't match
    pub async fn mark_genesis_incompatible(
        &self,
        peer_ip: &str,
        our_genesis: &str,
        their_genesis: &str,
    ) {
        let ip_only = extract_ip(peer_ip).to_string();
        let mut incompatible = self.incompatible_peers.write().await;

        if !incompatible.contains_key(&ip_only) {
            tracing::error!(
                "🚫 ═══════════════════════════════════════════════════════════════════"
            );
            tracing::error!("🚫 INCOMPATIBLE PEER DETECTED: {}", ip_only);
            tracing::error!("🚫 Reason: Genesis hash mismatch");
            tracing::error!("🚫   Our genesis:   {}", our_genesis);
            tracing::error!("🚫   Their genesis: {}", their_genesis);
            tracing::error!("🚫 ");
            tracing::error!("🚫 This peer is computing different block hashes, likely due to");
            tracing::error!("🚫 running an older version of the software.");
            tracing::error!("🚫 ");
            tracing::error!("🚫 RECOMMENDATION: The peer should update to the latest version");
            tracing::error!("🚫 and clear their blockchain to resync.");
            tracing::error!("🚫 ");
            tracing::error!("🚫 Genesis mismatch is PERMANENT - peer will NEVER be rechecked.");
            tracing::error!(
                "🚫 ═══════════════════════════════════════════════════════════════════"
            );
        }

        let reason = format!(
            "Genesis hash mismatch: ours={}, theirs={}",
            our_genesis, their_genesis
        );
        // Genesis mismatch is PERMANENT - these peers will never sync correctly
        incompatible.insert(ip_only, (std::time::Instant::now(), reason, true));
    }

    /// Verify genesis hash compatibility with a peer
    /// Returns true if compatible (same genesis hash), false if incompatible
    /// If incompatible, marks the peer as such
    pub async fn verify_genesis_compatibility(
        &self,
        peer_ip: &str,
        our_genesis_hash: [u8; 32],
    ) -> bool {
        let ip_only = extract_ip(peer_ip);

        // Request the peer's genesis block hash
        let request = NetworkMessage::GetBlockHash(0);

        match self.send_and_await_response(peer_ip, request, 10).await {
            Ok(NetworkMessage::BlockHashResponse {
                height: 0,
                hash: Some(their_hash),
            }) => {
                if our_genesis_hash == their_hash {
                    tracing::info!(
                        "✅ Genesis hash matches with peer {} - compatible for fork resolution",
                        ip_only
                    );
                    // Reset fork errors since they're compatible
                    self.reset_fork_errors(peer_ip);
                    // Record positive confirmation so sync skips re-verification
                    self.mark_genesis_confirmed(peer_ip).await;
                    true
                } else {
                    let our_hex = hex::encode(&our_genesis_hash[..8]);
                    let their_hex = hex::encode(&their_hash[..8]);

                    tracing::error!(
                        "🚫 Genesis hash MISMATCH with peer {} - incompatible software!",
                        ip_only
                    );
                    tracing::error!("🚫   Our genesis:   {}...", our_hex);
                    tracing::error!("🚫   Their genesis: {}...", their_hex);

                    // Mark as truly incompatible
                    self.mark_genesis_incompatible(peer_ip, &our_hex, &their_hex)
                        .await;
                    false
                }
            }
            Ok(NetworkMessage::BlockHashResponse {
                height: 0,
                hash: None,
            }) => {
                // Old code returns None hash for height 0 — assume compatible.
                tracing::debug!(
                    "ℹ️  Peer {} returned no genesis hash (old code): assuming compatible",
                    ip_only
                );
                self.mark_genesis_confirmed(peer_ip).await;
                true
            }
            Ok(other) => {
                // Unexpected message type — old code responding to GetGenesisHash with
                // whatever message it had queued.  Assume compatible, don't penalise.
                tracing::debug!(
                    "ℹ️  Unexpected genesis response from {} ({:?}): assuming compatible (old code)",
                    ip_only,
                    other.message_type()
                );
                self.mark_genesis_confirmed(peer_ip).await;
                true
            }
            Err(e) => {
                // Timeout or channel error: the peer is running software that predates the
                // GetGenesisHash message.  This is NOT evidence of an incompatible chain —
                // it simply means the peer hasn't been upgraded yet.  Treating timeout as
                // incompatible causes a complete network partition on startup because ALL
                // pre-upgrade nodes would be banned simultaneously.
                //
                // Policy: timeout → assume compatible.  Only mark incompatible if the peer
                // actually replies with a DIFFERENT genesis hash (strong proof of wrong chain).
                tracing::debug!(
                    "ℹ️  No genesis hash response from {} ({}): assuming compatible (old software)",
                    ip_only,
                    e
                );
                self.mark_genesis_confirmed(peer_ip).await;
                true
            }
        }
    }

    /// Mark a peer's genesis hash as confirmed (same chain as us).
    /// Called by verify_genesis_compatibility when the peer's hash matches ours,
    /// and by handle_genesis_hash_response when a GenesisHashResponse is verified.
    pub async fn mark_genesis_confirmed(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip);
        self.genesis_confirmed_peers
            .write()
            .await
            .insert(ip_only.to_string());
    }

    /// Returns true if this peer's genesis hash has been positively confirmed.
    pub async fn is_genesis_confirmed(&self, peer_ip: &str) -> bool {
        let ip_only = extract_ip(peer_ip);
        self.genesis_confirmed_peers.read().await.contains(ip_only)
    }

    /// Attempt to claim a genesis verification slot for this peer.
    /// Returns true if the caller should proceed with verification (slot was free and not in
    /// cooldown). Returns false if another task is already running or the peer was recently
    /// checked and timed out (prevents permanent flooding of old-code nodes).
    pub fn claim_genesis_check(&self, peer_ip: &str) -> bool {
        const GENESIS_CHECK_COOLDOWN_SECS: u64 = 300; // 5 minutes between retries after timeout
        let ip_only = extract_ip(peer_ip);
        // Enforce cooldown: skip if we already tried and the peer didn't respond
        if let Some(last) = self.genesis_check_last_attempt.get(ip_only) {
            if last.elapsed().as_secs() < GENESIS_CHECK_COOLDOWN_SECS {
                return false;
            }
        }
        self.pending_genesis_checks.insert(ip_only.to_string())
    }

    /// Release the genesis verification slot for this peer (call when verification completes).
    /// Records the attempt timestamp so cooldown applies on timeout/failure.
    pub fn release_genesis_check(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip);
        self.pending_genesis_checks.remove(ip_only);
        self.genesis_check_last_attempt
            .insert(ip_only.to_string(), std::time::Instant::now());
    }

    /// Clear the genesis check cooldown for a peer (called when peer reconnects or is confirmed).
    pub fn clear_genesis_check_cooldown(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip);
        self.genesis_check_last_attempt.remove(ip_only);
    }

    /// Get list of whitelisted peer IPs
    pub fn get_whitelisted_peers(&self) -> Vec<String> {
        // For now, return empty vec since whitelisting is checked per-peer
        // In the future, could maintain a cached list
        vec![]
    }

    /// Return the subset of currently-connected peers that are whitelisted
    /// (i.e. from the time-coin.io trusted peer list).
    ///
    /// Used during catch-up sync (AV31 fix) to prefer trusted peers when
    /// requesting blocks — an attacker who controls the majority of a node's
    /// initial connections cannot inject a forked chain if we ask whitelisted
    /// peers first.
    pub async fn get_whitelisted_connected_peers(&self) -> Vec<String> {
        let connected = self.get_connected_peers().await;
        let mut result = Vec::new();
        for ip in connected {
            if self.is_whitelisted(&ip).await {
                result.push(ip);
            }
        }
        result
    }

    /// Get list of compatible connected peers (excludes currently incompatible ones)
    pub async fn get_compatible_peers(&self) -> Vec<String> {
        // Return cached result if still fresh
        {
            let cache = self.compatible_peers_cache.read().await;
            if cache.1.elapsed().as_secs() < Self::COMPATIBLE_PEERS_CACHE_TTL && !cache.0.is_empty()
            {
                return cache.0.clone();
            }
        }

        // Cache miss — recompute
        let result = self.get_compatible_peers_uncached().await;

        // Store in cache
        {
            let mut cache = self.compatible_peers_cache.write().await;
            *cache = (result.clone(), std::time::Instant::now());
        }

        result
    }

    /// Uncached implementation of compatible peer computation
    async fn get_compatible_peers_uncached(&self) -> Vec<String> {
        // First, clean up expired incompatible entries (but NOT permanent ones)
        {
            let mut incompatible = self.incompatible_peers.write().await;
            incompatible.retain(|ip, (marked_at, _reason, permanent)| {
                // Permanent entries (genesis mismatch) are NEVER cleaned up
                if *permanent {
                    return true;
                }
                let expired = marked_at.elapsed().as_secs() >= Self::INCOMPATIBLE_RECHECK_SECS;
                if expired {
                    tracing::info!("🔄 Incompatible timeout expired for {}, will re-check", ip);
                }
                !expired
            });
        }

        let incompatible = self.incompatible_peers.read().await;
        // Live writer channels are the routing source of truth for send_to_peer().
        let all_connections = self.get_connected_peers().await;
        let compatible: Vec<String> = all_connections
            .iter()
            .filter(|ip| !incompatible.contains_key(extract_ip(ip)))
            .cloned()
            .collect();

        // Rate-limited logging for incompatible peers (once per 60 seconds)
        if !incompatible.is_empty() {
            static LAST_INCOMPATIBLE_LOG: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let last_log = LAST_INCOMPATIBLE_LOG.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last_log >= 60 {
                LAST_INCOMPATIBLE_LOG.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    "⚠️ Incompatible peers: {} marked, {} in connections, {} compatible",
                    incompatible.len(),
                    all_connections.len(),
                    compatible.len()
                );
                let mut old_code_peers: Vec<&str> = Vec::new();
                for (ip, (marked_at, reason, permanent)) in incompatible.iter() {
                    let status = if *permanent { "PERMANENT" } else { "temporary" };
                    tracing::warn!(
                        "  🚫 {} - {} [{}] ({}s ago)",
                        ip,
                        reason,
                        status,
                        marked_at.elapsed().as_secs()
                    );
                    if reason.contains("old code") {
                        old_code_peers.push(ip.as_str());
                    }
                }
                if !old_code_peers.is_empty() {
                    tracing::warn!(
                        "🔔 UPDATE REQUIRED: {} peer(s) appear to be running outdated software \
                        and cannot participate in genesis verification or fork resolution: {}. \
                        Please upgrade to timed v{} — https://github.com/time-coin/time-masternode",
                        old_code_peers.len(),
                        old_code_peers.join(", "),
                        env!("CARGO_PKG_VERSION"),
                    );
                }
            }
        }

        compatible
    }

    /// Get count of incompatible peers (for monitoring)
    pub async fn incompatible_count(&self) -> usize {
        self.incompatible_peers.read().await.len()
    }
}
