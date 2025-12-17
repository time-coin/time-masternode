use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Tracks which IPs we have active connections to (prevents duplicate connections)
/// Shared between client (outbound) and server (inbound) to detect duplicates
pub struct ConnectionManager {
    connected_ips: Arc<RwLock<HashSet<String>>>,
    inbound_ips: Arc<RwLock<HashSet<String>>>, // Track inbound connections separately
    reconnecting: Arc<RwLock<HashMap<String, ReconnectionState>>>, // Track backoff state
}

/// State for tracking reconnection backoff
#[derive(Clone)]
struct ReconnectionState {
    next_attempt: Instant,
    #[allow(dead_code)]
    attempt_count: u64,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connected_ips: Arc::new(RwLock::new(HashSet::new())),
            inbound_ips: Arc::new(RwLock::new(HashSet::new())),
            reconnecting: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Mark that we're connecting to this IP (outbound)
    /// Returns true if successfully marked (wasn't already connecting)
    /// Returns false if already connecting (prevents duplicate connection attempts)
    pub async fn mark_connecting(&self, ip: &str) -> bool {
        let mut ips = self.connected_ips.write().await;
        let inbound = self.inbound_ips.read().await;

        // Check if already connected in either direction
        if ips.contains(ip) || inbound.contains(ip) {
            return false;
        }

        // Insert and return true only if it's new
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

    /// Force remove connection (used when accepting inbound over outbound)
    pub async fn remove(&self, ip: &str) {
        let mut outbound = self.connected_ips.write().await;
        let mut inbound = self.inbound_ips.write().await;
        outbound.remove(ip);
        inbound.remove(ip);
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

    /// Mark that a peer is in reconnection backoff
    /// This prevents duplicate connection attempts during backoff period
    pub async fn mark_reconnecting(&self, ip: &str, retry_delay: u64, attempt_count: u64) {
        let mut reconnecting = self.reconnecting.write().await;
        reconnecting.insert(
            ip.to_string(),
            ReconnectionState {
                next_attempt: Instant::now() + std::time::Duration::from_secs(retry_delay),
                attempt_count,
            },
        );
    }

    /// Check if a peer is in reconnection backoff
    /// Returns true if we should skip connecting (still in backoff)
    pub async fn is_reconnecting(&self, ip: &str) -> bool {
        let reconnecting = self.reconnecting.read().await;
        if let Some(state) = reconnecting.get(ip) {
            // Check if backoff period has elapsed
            Instant::now() < state.next_attempt
        } else {
            false
        }
    }

    /// Clear reconnection state when connection succeeds or is abandoned
    pub async fn clear_reconnecting(&self, ip: &str) {
        let mut reconnecting = self.reconnecting.write().await;
        reconnecting.remove(ip);
    }
}
