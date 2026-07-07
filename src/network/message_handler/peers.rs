use super::context::MessageContext;
use super::MessageHandler;
use crate::network::message::NetworkMessage;
use tracing::{debug, info};

impl MessageHandler {
    pub(super) async fn handle_get_peers(
        &self,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received GetPeers request from {}",
            self.direction, self.peer_ip
        );

        // Build load-aware list and enrich each entry with tier info from the registry
        let mut entries = context.peer_registry.get_peers_by_load(32).await;
        for entry in entries.iter_mut() {
            if let Some(info) = context.masternode_registry.get(&entry.address).await {
                entry.is_masternode = true;
                entry.tier = Some(info.masternode.tier);
            }
        }

        // Include ourselves
        let our_count = context.peer_registry.connected_count() as u16;
        let our_ip = context.peer_registry.get_local_ip().unwrap_or_default();
        let our_tier = if let Some(ip) = context.peer_registry.get_local_ip() {
            context
                .masternode_registry
                .get(&ip)
                .await
                .map(|i| i.masternode.tier)
        } else {
            None
        };
        if !our_ip.is_empty() {
            entries.push(crate::network::message::PeerExchangeEntry {
                address: our_ip,
                connection_count: our_count,
                is_masternode: our_tier.is_some(),
                tier: our_tier,
            });
        }

        // Sort by tier priority (Gold first) then by load within each tier
        entries.sort_by(|a, b| {
            let tier_ord = |t: &Option<crate::types::MasternodeTier>| match t {
                Some(crate::types::MasternodeTier::Gold) => 0u8,
                Some(crate::types::MasternodeTier::Silver) => 1,
                Some(crate::types::MasternodeTier::Bronze) => 2,
                Some(crate::types::MasternodeTier::Free) => 3,
                None => 4,
            };
            tier_ord(&a.tier)
                .cmp(&tier_ord(&b.tier))
                .then(a.connection_count.cmp(&b.connection_count))
        });

        debug!(
            "📤 [{}] Sending PeerExchange ({} peers, tier-sorted) to {}",
            self.direction,
            entries.len(),
            self.peer_ip
        );
        Ok(Some(NetworkMessage::PeerExchange(entries)))
    }

    /// Handle incoming PeerExchange — store load data and add new peers as candidates.
    pub(super) async fn handle_peer_exchange(
        &self,
        entries: Vec<crate::network::message::PeerExchangeEntry>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received PeerExchange from {} ({} entries)",
            self.direction,
            self.peer_ip,
            entries.len()
        );

        let mut added = 0usize;
        for entry in &entries {
            // Store load info so Phase 3 can prefer less-loaded peers
            context
                .peer_registry
                .update_peer_load(&entry.address, entry.connection_count);

            // Add as peer candidate if new
            if let Some(peer_manager) = &context.peer_manager {
                if peer_manager.add_peer_candidate(entry.address.clone()).await {
                    added += 1;
                }
            } else {
                context
                    .peer_registry
                    .add_discovered_peers(std::slice::from_ref(&entry.address))
                    .await;
            }
        }
        if added > 0 {
            info!(
                "✓ [{}] Added {} new peer candidate(s) from PeerExchange ({})",
                self.direction, added, self.peer_ip
            );
        }
        Ok(None)
    }

    /// Handle PeersResponse
    pub(super) async fn handle_peers_response(
        &self,
        peers: Vec<String>,
        context: &MessageContext,
    ) -> Result<Option<NetworkMessage>, String> {
        debug!(
            "📥 [{}] Received PeersResponse from {} with {} peer(s)",
            self.direction,
            self.peer_ip,
            peers.len()
        );

        // Add to peer_manager if available
        if let Some(peer_manager) = &context.peer_manager {
            let mut added = 0;
            for peer_addr in &peers {
                if peer_manager.add_peer_candidate(peer_addr.clone()).await {
                    added += 1;
                }
            }
            if added > 0 {
                info!(
                    "✓ [{}] Added {} new peer candidate(s) from {}",
                    self.direction, added, self.peer_ip
                );
            }
        } else {
            // Fallback to peer_registry discovered_peers
            context.peer_registry.add_discovered_peers(&peers).await;
        }

        Ok(None)
    }
}
