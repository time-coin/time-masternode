//! Peer discovery service for finding peers from external sources.
//!
//! Fetches peer IPs from the time-coin.io API:
//! - Testnet: https://time-coin.io/api/testnet/peers
//! - Mainnet: https://time-coin.io/api/peers
//!
//! The API returns IP addresses without ports. The default port is
//! determined by the network type (24100 for testnet, 24000 for mainnet).
//!
//! Note: Masternodes are discovered via P2P protocol (GetMasternodes message),
//! not through a separate API endpoint. The /api/peers endpoint returns all
//! peers, which may include masternodes.
//!
//! Note: This module appears as "dead code" in library checks because it's
//! only used by the binary (main.rs).

#![allow(dead_code)]

use crate::network_type::NetworkType;

/// Peer discovery service for finding peers from external sources
pub struct PeerDiscovery {
    discovery_url: String,
    default_port: u16,
}

impl PeerDiscovery {
    /// Create a new peer discovery service with network-specific settings
    pub fn new(discovery_url: String, network_type: NetworkType) -> Self {
        Self {
            discovery_url,
            default_port: network_type.default_p2p_port(),
        }
    }

    /// Fetch peers from the discovery service with fallback to bootstrap peers
    ///
    /// 1. Tries to fetch from the discovery API
    /// 2. Falls back to bootstrap peers if the request fails
    /// 3. Returns peers with network-appropriate default port for IPs without ports
    pub async fn fetch_peers_with_fallback(
        &self,
        fallback_peers: Vec<String>,
    ) -> Vec<DiscoveredPeer> {
        // Try to fetch from API
        match self.fetch_from_api().await {
            Ok(peers) if !peers.is_empty() => peers,
            Ok(_) => {
                tracing::debug!("API returned empty list, using fallback peers");
                self.parse_peer_list(fallback_peers)
            }
            Err(e) => {
                tracing::debug!("API fetch failed: {}, using fallback peers", e);
                self.parse_peer_list(fallback_peers)
            }
        }
    }

    /// Fetch peers from the discovery API
    async fn fetch_from_api(&self) -> Result<Vec<DiscoveredPeer>, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

        let response = client
            .get(&self.discovery_url)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let peer_list: Vec<String> = response.json().await.map_err(|e| e.to_string())?;

        Ok(self.parse_peer_list(peer_list))
    }

    /// Parse a list of peer addresses (IP or IP:port format)
    fn parse_peer_list(&self, peers: Vec<String>) -> Vec<DiscoveredPeer> {
        peers
            .into_iter()
            .filter_map(|peer_str| {
                let peer_str = peer_str.trim();
                if peer_str.is_empty() {
                    return None;
                }

                // Check if it has a port (contains ':' and last part is numeric)
                if let Some(colon_pos) = peer_str.rfind(':') {
                    let potential_port = &peer_str[colon_pos + 1..];
                    if let Ok(port) = potential_port.parse::<u16>() {
                        let address = peer_str[..colon_pos].to_string();
                        // Filter invalid addresses
                        if Self::is_invalid_address(&address) {
                            tracing::debug!("🚫 Filtered invalid peer address: {}", address);
                            return None;
                        }
                        return Some(DiscoveredPeer { address, port });
                    }
                }

                // No port or invalid port - use network default port
                // Filter invalid addresses
                if Self::is_invalid_address(peer_str) {
                    tracing::debug!("🚫 Filtered invalid peer address: {}", peer_str);
                    return None;
                }

                Some(DiscoveredPeer {
                    address: peer_str.to_string(),
                    port: self.default_port,
                })
            })
            .collect()
    }

    /// Check if an address is invalid (localhost, 0.0.0.0, etc.)
    fn is_invalid_address(addr: &str) -> bool {
        addr == "0.0.0.0"
            || addr == "127.0.0.1"
            || addr.starts_with("127.")
            || addr.starts_with("0.0.0.")
            || addr.is_empty()
    }
}

/// Represents a discovered peer
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub address: String,
    pub port: u16,
}
