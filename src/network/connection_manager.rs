use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Tracks which IPs we have active connections to (prevents duplicate connections)
/// Shared between client (outbound) and server (inbound) to detect duplicates
pub struct ConnectionManager {
    connected_ips: Arc<RwLock<HashSet<String>>>,
    inbound_ips: Arc<RwLock<HashSet<String>>>, // Track inbound connections separately
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connected_ips: Arc::new(RwLock::new(HashSet::new())),
            inbound_ips: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Mark that we're connecting to this IP (outbound)
    pub async fn mark_connecting(&self, ip: &str) -> bool {
        let mut ips = self.connected_ips.write().await;
        ips.insert(ip.to_string())
    }

    /// Check if we're already connected/connecting to this IP (either direction)
    pub async fn is_connected(&self, ip: &str) -> bool {
        let outbound = self.connected_ips.read().await;
        let inbound = self.inbound_ips.read().await;
        outbound.contains(ip) || inbound.contains(ip)
    }

    /// Mark an inbound connection
    #[allow(dead_code)]
    pub async fn mark_inbound(&self, ip: &str) -> bool {
        let mut ips = self.inbound_ips.write().await;
        ips.insert(ip.to_string())
    }

    /// Remove IP when connection ends (outbound)
    pub async fn mark_disconnected(&self, ip: &str) {
        let mut ips = self.connected_ips.write().await;
        ips.remove(ip);
    }

    /// Remove IP when inbound connection ends
    #[allow(dead_code)]
    pub async fn mark_inbound_disconnected(&self, ip: &str) {
        let mut ips = self.inbound_ips.write().await;
        ips.remove(ip);
    }

    /// Get count of connected peers (both directions)
    pub async fn connected_count(&self) -> usize {
        let outbound = self.connected_ips.read().await;
        let inbound = self.inbound_ips.read().await;
        // Count unique IPs across both sets
        let mut all_ips = outbound.clone();
        all_ips.extend(inbound.iter().cloned());
        all_ips.len()
    }
}
