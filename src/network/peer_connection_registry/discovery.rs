use super::types::extract_ip;
use super::PeerConnectionRegistry;
use tracing::debug;

impl PeerConnectionRegistry {
    /// Add discovered peers from peer exchange
    pub async fn add_discovered_peers(&self, peers: &[String]) {
        let mut discovered = self.discovered_peers.write().await;
        let mut added = 0;
        for peer in peers {
            // Extract IP only (remove port if present)
            let ip = extract_ip(peer);
            if discovered.insert(ip.to_string()) {
                added += 1;
            }
        }
        if added > 0 {
            debug!("📥 Added {} new discovered peer candidate(s)", added);
        }
    }

    /// Get and clear discovered peers (for network client to process)
    pub async fn take_discovered_peers(&self) -> Vec<String> {
        let mut discovered = self.discovered_peers.write().await;
        let peers: Vec<String> = discovered.drain().collect();
        peers
    }

    /// Get discovered peers count
    pub async fn discovered_peers_count(&self) -> usize {
        self.discovered_peers.read().await.len()
    }

    /// Record the reported connection count for a peer (from PeerExchange messages).
    /// Used for load-aware peer selection so nodes can steer new connections toward
    /// less-loaded masternodes instead of always hitting the same bootstrap nodes.
    pub fn update_peer_load(&self, ip: &str, connection_count: u16) {
        self.peer_load.insert(ip.to_string(), connection_count);
    }

    /// Get the last-reported connection count for a peer, or u16::MAX if unknown.
    /// Returning MAX for unknown peers causes them to sort to the back, so we prefer
    /// known-underloaded peers while still eventually trying unknown ones.
    pub fn get_peer_load(&self, ip: &str) -> u16 {
        self.peer_load.get(ip).map(|v| *v).unwrap_or(u16::MAX)
    }

    /// Build a PeerExchange list of currently connected peers, sorted by ascending
    /// connection load, capped at `limit` entries.  Callers use this to respond to
    /// GetPeers requests and to redirect overloaded inbound connections.
    /// Note: `tier` is left as None here — callers with registry access fill it in.
    pub async fn get_peers_by_load(
        &self,
        limit: usize,
    ) -> Vec<crate::network::message::PeerExchangeEntry> {
        let connected = self.get_connected_peers().await;
        let mut entries: Vec<_> = connected
            .into_iter()
            .map(|ip| {
                let count = self.get_peer_load(&ip);
                let is_mn = self.peer_load.contains_key(ip.as_str());
                crate::network::message::PeerExchangeEntry {
                    address: ip,
                    connection_count: count,
                    is_masternode: is_mn,
                    tier: None, // filled in by callers that have masternode registry access
                }
            })
            .collect();
        // Sort ascending by load — least loaded first
        entries.sort_by_key(|e| e.connection_count);
        entries.truncate(limit);
        entries
    }
}
