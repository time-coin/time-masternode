use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Tracks which IPs we have active connections to (prevents duplicate connections)
pub struct ConnectionManager {
    connected_ips: Arc<RwLock<HashSet<String>>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connected_ips: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Mark that we're connecting to this IP
    pub async fn mark_connecting(&self, ip: &str) -> bool {
        let mut ips = self.connected_ips.write().await;
        ips.insert(ip.to_string())
    }

    /// Check if we're already connected/connecting to this IP
    pub async fn is_connected(&self, ip: &str) -> bool {
        let ips = self.connected_ips.read().await;
        ips.contains(ip)
    }

    /// Remove IP when connection ends
    pub async fn mark_disconnected(&self, ip: &str) {
        let mut ips = self.connected_ips.write().await;
        ips.remove(ip);
    }
}
