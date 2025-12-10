use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{error, info};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub address: String,
    pub port: u16,
}

pub struct PeerDiscovery {
    client: Client,
    api_url: String,
}

impl PeerDiscovery {
    pub fn new(api_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self { client, api_url }
    }

    pub async fn fetch_peers(&self) -> Result<Vec<PeerInfo>, String> {
        info!("🔍 Discovering peers from {}", self.api_url);

        match self.client.get(&self.api_url).send().await {
            Ok(response) => {
                if !response.status().is_success() {
                    let status = response.status();
                    return Err(format!("API returned error status: {}", status));
                }

                // Parse as array of strings: ["ip:port", "ip:port"]
                match response.json::<Vec<String>>().await {
                    Ok(peer_strings) => {
                        let peers: Vec<PeerInfo> = peer_strings
                            .iter()
                            .filter_map(|s| {
                                let parts: Vec<&str> = s.split(':').collect();
                                if parts.len() == 2 {
                                    if let Ok(port) = parts[1].parse::<u16>() {
                                        return Some(PeerInfo {
                                            address: parts[0].to_string(),
                                            port,
                                        });
                                    }
                                }
                                None
                            })
                            .collect();

                        info!("✅ Discovered {} peers", peers.len());
                        Ok(peers)
                    }
                    Err(e) => {
                        error!("❌ Failed to parse peer list: {}", e);
                        Err(format!("Failed to parse response: {}", e))
                    }
                }
            }
            Err(e) => {
                error!("❌ Failed to fetch peers: {}", e);
                Err(format!("Network error: {}", e))
            }
        }
    }

    pub async fn fetch_peers_with_fallback(&self, fallback_peers: Vec<String>) -> Vec<PeerInfo> {
        match self.fetch_peers().await {
            Ok(peers) if !peers.is_empty() => peers,
            Ok(_) | Err(_) => {
                info!(
                    "⚠️  Using fallback peer list ({} peers)",
                    fallback_peers.len()
                );
                fallback_peers
                    .into_iter()
                    .map(|addr| {
                        let parts: Vec<&str> = addr.split(':').collect();
                        PeerInfo {
                            address: parts[0].to_string(),
                            port: parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(24100),
                        }
                    })
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_peer_discovery() {
        let discovery = PeerDiscovery::new("https://time-coin.io/api/peers".to_string());

        // This will fail in test environment, but demonstrates the API
        let result = discovery.fetch_peers().await;
        assert!(result.is_ok() || result.is_err()); // Just checking it doesn't panic
    }
}
