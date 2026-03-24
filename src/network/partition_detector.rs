//! Detects when this node is isolated from the network and triggers recovery.
//!
//! A partition is declared when ALL of the following are true:
//!   - peer_count == 0 for >= ISOLATION_THRESHOLD_SECS (30s)
//!   - last block received was >= BLOCK_STALL_SECS ago (60s)
//!   - at least one recovery attempt has failed
//!
//! Recovery sequence (on each alarm tick):
//!   1. Fetch fresh peers from the discovery API
//!   2. Try bootstrap_peers from config
//!   3. Try all known masternodes from registry (list_all())
//!   4. Log the partition state clearly

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::{Duration, Instant};

use crate::masternode_registry::MasternodeRegistry;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::NetworkType;

pub const ISOLATION_THRESHOLD_SECS: u64 = 30;
pub const RECOVERY_INTERVAL_SECS: u64 = 60;
pub const BLOCK_STALL_SECS: u64 = 60;

pub struct PartitionDetector {
    peer_registry: Arc<PeerConnectionRegistry>,
    masternode_registry: Arc<MasternodeRegistry>,
    peer_manager: Arc<PeerManager>,
    bootstrap_peers: Vec<String>,
    network_type: NetworkType,
    last_block_time: Arc<AtomicU64>,
    is_partitioned: Arc<AtomicBool>,
    local_ip: Option<String>,
}

impl PartitionDetector {
    pub fn new(
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
        peer_manager: Arc<PeerManager>,
        bootstrap_peers: Vec<String>,
        network_type: NetworkType,
        last_block_time: Arc<AtomicU64>,
        local_ip: Option<String>,
    ) -> Self {
        Self {
            peer_registry,
            masternode_registry,
            peer_manager,
            bootstrap_peers,
            network_type,
            last_block_time,
            is_partitioned: Arc::new(AtomicBool::new(false)),
            local_ip,
        }
    }

    /// Returns whether this node is currently considered partitioned from the network.
    pub fn is_partitioned(&self) -> bool {
        self.is_partitioned.load(Ordering::Relaxed)
    }

    /// Call this every time a new block arrives to reset the stall timer.
    pub fn record_block_received(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_block_time.store(now, Ordering::Relaxed);
    }

    /// Main background loop — spawn with tokio::spawn.
    pub async fn run(self) {
        // Wait past the isolation threshold so a freshly started node doesn't
        // immediately trigger a false-positive partition alarm.
        tokio::time::sleep(Duration::from_secs(ISOLATION_THRESHOLD_SECS + 5)).await;

        let mut isolation_start: Option<Instant> = None;
        let mut partition_start: Option<Instant> = None;
        let mut attempt_count: u64 = 0;

        // Poll every 10 s so we detect conditions quickly without spinning.
        let mut interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            interval.tick().await;

            let peer_count = self.peer_registry.peer_count().await;
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last_block = self.last_block_time.load(Ordering::Relaxed);
            let block_age_secs = now_unix.saturating_sub(last_block);

            if peer_count == 0 {
                // Start (or continue) tracking isolation duration.
                let iso_start = isolation_start.get_or_insert_with(Instant::now);
                let isolated_secs = iso_start.elapsed().as_secs();

                // Declare partition only after both thresholds are exceeded.
                if isolated_secs >= ISOLATION_THRESHOLD_SECS && block_age_secs >= BLOCK_STALL_SECS {
                    if !self.is_partitioned.load(Ordering::Relaxed) {
                        self.is_partitioned.store(true, Ordering::Relaxed);
                        partition_start = Some(Instant::now());
                        tracing::warn!(
                            "🔌 NETWORK PARTITION DETECTED — 0 peers for {}s, last block {}s ago",
                            isolated_secs,
                            block_age_secs
                        );
                    }

                    // Attempt recovery once immediately, then every RECOVERY_INTERVAL_SECS.
                    let elapsed_since_partition = partition_start
                        .map(|ps| ps.elapsed().as_secs())
                        .unwrap_or(0);

                    let should_attempt = attempt_count == 0
                        || elapsed_since_partition >= attempt_count * RECOVERY_INTERVAL_SECS;

                    if should_attempt {
                        attempt_count += 1;
                        if attempt_count > 1 {
                            tracing::warn!(
                                "🔌 Still partitioned (attempt {}/∞) — 0 peers connected",
                                attempt_count
                            );
                        }
                        self.attempt_recovery().await;
                    }
                }
            } else {
                // We have at least one peer — clear partition state.
                if self.is_partitioned.load(Ordering::Relaxed) {
                    let isolated_duration = partition_start
                        .map(|ps| ps.elapsed().as_secs())
                        .unwrap_or(0);
                    tracing::info!(
                        "✅ Partition resolved — reconnected to {} peers after {}s isolation",
                        peer_count,
                        isolated_duration
                    );
                    self.is_partitioned.store(false, Ordering::Relaxed);
                    partition_start = None;
                    attempt_count = 0;
                }
                // Reset isolation timer whenever we have peers.
                isolation_start = None;
            }
        }
    }

    /// Gather all candidate addresses and register them as peer candidates so
    /// the running NetworkClient will attempt connections on its next cycle.
    /// Returns true if any candidates were queued.
    async fn attempt_recovery(&self) -> bool {
        let default_port = self.network_type.default_p2p_port();
        let mut candidates: Vec<String> = Vec::new();

        // 1. Fetch fresh peers from the discovery API.
        let discovery_url = self.network_type.peer_discovery_url();
        let client = crate::http_client::HttpClient::new()
            .with_timeout(std::time::Duration::from_secs(10))
            .with_accept_invalid_certs(true);
        match client.get(discovery_url).await {
            Ok(resp) => {
                if let Ok(peers) = resp.json::<Vec<String>>() {
                    for peer in peers {
                        let addr = if peer.contains(':') {
                            peer
                        } else {
                            format!("{}:{}", peer, default_port)
                        };
                        candidates.push(addr);
                    }
                }
            }
            Err(e) => {
                tracing::debug!("Partition recovery: discovery API unreachable: {}", e);
            }
        }

        // 2. Add bootstrap peers from config.
        for peer in &self.bootstrap_peers {
            let addr = if peer.contains(':') {
                peer.clone()
            } else {
                format!("{}:{}", peer, default_port)
            };
            candidates.push(addr);
        }

        // 3. Add all known masternodes from the registry.
        let masternodes = self.masternode_registry.list_all().await;
        for info in &masternodes {
            let addr = &info.masternode.address;
            let addr_with_port = if addr.contains(':') {
                addr.clone()
            } else {
                format!("{}:{}", addr, default_port)
            };
            candidates.push(addr_with_port);
        }

        candidates.sort();
        candidates.dedup();

        // Filter out self-connections
        if let Some(ref local_ip) = self.local_ip {
            candidates.retain(|addr| {
                let ip = addr.split(':').next().unwrap_or(addr.as_str());
                ip != local_ip.as_str()
            });
        }

        tracing::info!(
            "🔄 Partition recovery: trying {} bootstrap peers, {} known masternodes",
            self.bootstrap_peers.len(),
            masternodes.len()
        );

        if candidates.is_empty() {
            return false;
        }

        // Register each candidate with the peer manager; the NetworkClient will
        // pick them up and attempt TCP connections on its next loop iteration.
        for addr in candidates {
            let pm = self.peer_manager.clone();
            tokio::spawn(async move {
                pm.add_peer_candidate(addr).await;
            });
        }

        true
    }
}
