use crate::config::NetworkConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};

const PEER_DISCOVERY_URL: &str = "https://time-coin.io/api/peers";
const PEER_DISCOVERY_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const PEER_REFRESH_INTERVAL: Duration = Duration::from_secs(300); // 5 minutes

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub address: String,
    pub last_seen: i64,
    pub version: String,
    pub is_masternode: bool,
    pub connection_attempts: u32,
    pub last_attempt: i64,
}

pub struct PeerManager {
    peers: Arc<RwLock<HashSet<String>>>,
    peer_info: Arc<RwLock<Vec<PeerInfo>>>,
    db: Arc<sled::Db>,
    network_config: NetworkConfig,
}

impl PeerManager {
    pub fn new(db: Arc<sled::Db>, network_config: NetworkConfig) -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashSet::new())),
            peer_info: Arc::new(RwLock::new(Vec::new())),
            db,
            network_config,
        }
    }

    /// Initialize peer manager - load from disk and discover new peers
    pub async fn initialize(&self) -> Result<(), String> {
        // Load peers from disk
        self.load_peers_from_disk().await?;

        // Discover peers from central server
        self.discover_peers_from_server().await?;

        // Start background tasks
        self.start_background_tasks().await;

        Ok(())
    }

    /// Load peers from sled database
    async fn load_peers_from_disk(&self) -> Result<(), String> {
        let tree = self.db.open_tree("peers").map_err(|e| e.to_string())?;

        let mut loaded_count = 0;
        for (key, value) in tree.iter().flatten() {
            if let Ok(peer_info) = bincode::deserialize::<PeerInfo>(&value) {
                let address = String::from_utf8_lossy(&key).to_string();
                self.peers.write().await.insert(address.clone());
                self.peer_info.write().await.push(peer_info);
                loaded_count += 1;
            }
        }

        info!("✓ Loaded {} peer(s) from disk", loaded_count);
        Ok(())
    }

    /// Discover peers from central server
    async fn discover_peers_from_server(&self) -> Result<(), String> {
        info!("🔍 Discovering peers from {}", PEER_DISCOVERY_URL);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;

        match client.get(PEER_DISCOVERY_URL).send().await {
            Ok(response) => {
                if let Ok(peer_list) = response.json::<Vec<String>>().await {
                    let mut added = 0;
                    for peer_addr in peer_list {
                        if self.add_peer_candidate(peer_addr.clone()).await {
                            added += 1;
                        }
                    }
                    info!("✓ Discovered {} new peer candidate(s) from server (will verify on connection)", added);
                    Ok(())
                } else {
                    warn!("⚠️  Failed to parse peer list from server");
                    Ok(())
                }
            }
            Err(e) => {
                warn!("⚠️  Failed to connect to discovery server: {}", e);
                Ok(())
            }
        }
    }

    /// Add a peer to the manager (only adds to candidate list, not saved until connection succeeds)
    pub async fn add_peer_candidate(&self, address: String) -> bool {
        let mut peers = self.peers.write().await;
        let is_new = peers.insert(address.clone());

        if is_new {
            // Also add to peer_info so get_all_peers() can find it
            let mut peer_info = self.peer_info.write().await;
            peer_info.push(PeerInfo {
                address,
                last_seen: 0, // Never connected yet
                version: "unknown".to_string(),
                is_masternode: false,
                connection_attempts: 0,
                last_attempt: 0,
            });
        }

        is_new
    }

    /// Add a verified peer (after successful connection)
    #[allow(dead_code)]
    pub async fn add_peer(&self, address: String) -> bool {
        let mut peers = self.peers.write().await;
        if peers.insert(address.clone()) {
            // New peer - add to info list and save to disk
            let peer_info = PeerInfo {
                address: address.clone(),
                last_seen: chrono::Utc::now().timestamp(),
                version: "unknown".to_string(),
                is_masternode: false,
                connection_attempts: 0,
                last_attempt: 0,
            };

            self.peer_info.write().await.push(peer_info.clone());

            // Save to disk only after successful connection
            if let Err(e) = self.save_peer_to_disk(&peer_info).await {
                error!("Failed to save peer to disk: {}", e);
            }

            info!("✓ Verified and saved peer: {}", address);
            true
        } else {
            false
        }
    }

    /// Save peer to sled database
    async fn save_peer_to_disk(&self, peer_info: &PeerInfo) -> Result<(), String> {
        let tree = self.db.open_tree("peers").map_err(|e| e.to_string())?;
        let key = peer_info.address.as_bytes();
        let value = bincode::serialize(peer_info).map_err(|e| e.to_string())?;
        tree.insert(key, value).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Update peer info when we connect
    #[allow(dead_code)]
    pub async fn update_peer_connected(&self, address: &str, version: String, is_masternode: bool) {
        let mut peer_info = self.peer_info.write().await;
        if let Some(info) = peer_info.iter_mut().find(|p| p.address == address) {
            info.last_seen = chrono::Utc::now().timestamp();
            info.version = version;
            info.is_masternode = is_masternode;

            // Save updated info
            if let Err(e) = self.save_peer_to_disk(info).await {
                error!("Failed to update peer on disk: {}", e);
            }
        }
    }

    /// Mark a connection attempt
    #[allow(dead_code)]
    pub async fn mark_connection_attempt(&self, address: &str, success: bool) {
        let mut peer_info = self.peer_info.write().await;
        if let Some(info) = peer_info.iter_mut().find(|p| p.address == address) {
            info.connection_attempts += 1;
            info.last_attempt = chrono::Utc::now().timestamp();

            if success {
                info.connection_attempts = 0; // Reset on success
            }

            // Save updated info
            if let Err(e) = self.save_peer_to_disk(info).await {
                error!("Failed to update peer on disk: {}", e);
            }
        }
    }

    /// Get list of all peers
    pub async fn get_peers(&self) -> Vec<String> {
        self.peers.read().await.iter().cloned().collect()
    }

    /// Get list of active masternodes
    #[allow(dead_code)]
    pub async fn get_masternodes(&self) -> Vec<String> {
        self.peer_info
            .read()
            .await
            .iter()
            .filter(|p| p.is_masternode)
            .map(|p| p.address.clone())
            .collect()
    }

    /// Get all peer addresses
    pub async fn get_all_peers(&self) -> Vec<String> {
        self.peer_info
            .read()
            .await
            .iter()
            .map(|p| p.address.clone())
            .collect()
    }

    /// Get count of connected masternodes
    #[allow(dead_code)]
    pub async fn masternode_count(&self) -> usize {
        self.peer_info
            .read()
            .await
            .iter()
            .filter(|p| p.is_masternode)
            .count()
    }

    /// Broadcast a message to all connected peers (deprecated - use NetworkServer broadcast instead)
    #[allow(dead_code)]
    pub async fn broadcast(&self, msg: crate::network::message::NetworkMessage) {
        // This would be implemented by the network layer
        // For now, we'll just log it
        tracing::debug!(
            "Would broadcast message to {} peers",
            self.peers.read().await.len()
        );
        // TODO: Implement actual broadcast through network connections
        let _ = msg; // Suppress unused warning
    }

    /// Request peer list from a connected peer
    pub async fn request_peers_from_peer(&self, _peer_address: &str) {
        // This would send a GetPeers message to the peer
        // For now, it's a placeholder that would be implemented
        // when we add the P2P communication layer
    }

    /// Start background maintenance tasks
    async fn start_background_tasks(&self) {
        let manager = self.clone_arc();

        // Task 1: Periodic discovery from server
        tokio::spawn(async move {
            let mut discovery_interval = interval(PEER_DISCOVERY_INTERVAL);
            loop {
                discovery_interval.tick().await;
                if let Err(e) = manager.discover_peers_from_server().await {
                    error!("Peer discovery failed: {}", e);
                }
            }
        });

        let manager = self.clone_arc();

        // Task 2: Request peers from connected peers
        tokio::spawn(async move {
            let mut refresh_interval = interval(PEER_REFRESH_INTERVAL);
            loop {
                refresh_interval.tick().await;
                let peers = manager.get_peers().await;
                for peer in peers.iter().take(5) {
                    // Request from up to 5 peers
                    manager.request_peers_from_peer(peer).await;
                }
            }
        });

        let manager = self.clone_arc();

        // Task 3: Cleanup stale peers
        tokio::spawn(async move {
            let mut cleanup_interval = interval(Duration::from_secs(3600)); // 1 hour
            loop {
                cleanup_interval.tick().await;
                manager.cleanup_stale_peers().await;
            }
        });
    }

    /// Remove peers that haven't been seen in a long time or have too many failed attempts
    async fn cleanup_stale_peers(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut peer_info = self.peer_info.write().await;
        let mut peers = self.peers.write().await;

        let mut removed = Vec::new();

        peer_info.retain(|info| {
            // Remove if not seen in 7 days or 10+ failed connection attempts
            let is_stale = now - info.last_seen > 7 * 24 * 3600 || info.connection_attempts > 10;
            if is_stale {
                removed.push(info.address.clone());
                peers.remove(&info.address);
            }
            !is_stale
        });

        if !removed.is_empty() {
            info!("🧹 Cleaned up {} stale peer(s)", removed.len());

            // Remove from disk
            if let Ok(tree) = self.db.open_tree("peers") {
                for addr in removed {
                    let _ = tree.remove(addr.as_bytes());
                }
            }
        }
    }

    /// Helper to clone Arc-wrapped self for spawning tasks
    fn clone_arc(&self) -> Arc<Self> {
        Arc::new(Self {
            peers: self.peers.clone(),
            peer_info: self.peer_info.clone(),
            db: self.db.clone(),
            network_config: self.network_config.clone(),
        })
    }
}

impl Clone for PeerManager {
    fn clone(&self) -> Self {
        Self {
            peers: self.peers.clone(),
            peer_info: self.peer_info.clone(),
            db: self.db.clone(),
            network_config: self.network_config.clone(),
        }
    }
}
