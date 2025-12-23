/// Peer discovery service for finding peers from external sources
/// Currently uses bootstrap peers as fallback

pub struct PeerDiscovery {
    discovery_url: String,
}

impl PeerDiscovery {
    /// Create a new peer discovery service
    pub fn new(discovery_url: String) -> Self {
        Self { discovery_url }
    }

    /// Fetch peers from the discovery service with fallback to bootstrap peers
    ///
    /// In a production system, this would:
    /// 1. Make an HTTP request to the discovery_url
    /// 2. Parse the response
    /// 3. Fall back to bootstrap peers if the request fails
    ///
    /// For now, we just return the bootstrap peers directly.
    pub async fn fetch_peers_with_fallback(
        &self,
        fallback_peers: Vec<String>,
    ) -> Vec<DiscoveredPeer> {
        // Convert bootstrap peer addresses to DiscoveredPeer format
        fallback_peers
            .into_iter()
            .filter_map(|peer_str| {
                // Parse "address:port" format
                let parts: Vec<&str> = peer_str.split(':').collect();
                if parts.len() == 2 {
                    if let Ok(port) = parts[1].parse::<u16>() {
                        Some(DiscoveredPeer {
                            address: parts[0].to_string(),
                            port,
                        })
                    } else {
                        None
                    }
                } else {
                    // If no port specified, use default P2P port
                    Some(DiscoveredPeer {
                        address: peer_str,
                        port: 24100, // Default testnet P2P port
                    })
                }
            })
            .collect()
    }
}

/// Represents a discovered peer
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub address: String,
    pub port: u16,
}
