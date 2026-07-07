use crate::block::types::Block;
use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::banlist::IPBanlist;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
// Add explicit imports
use std::sync::Arc;

pub(super) type OperatorMessages =
    Option<Arc<std::sync::Mutex<std::collections::VecDeque<(u64, String, String)>>>>;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

/// Global rate limiter for fork alert sending, keyed by peer IP.
/// Stores (last_alert_time, alert_count, peer_height_at_last_alert) to enable
/// exponential backoff: 60s → 2m → 5m → 10m cap. Resets when peer height changes.
pub(super) fn fork_alert_rate_limit() -> &'static dashmap::DashMap<String, (Instant, u32, u64)> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, (Instant, u32, u64)>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// Rate-limits GetBlocks requests sent from the ChainTip handler.
/// Without this, every block announcement from a far-ahead peer causes a new
/// GetBlocks request, which triggers the remote peer's sync-loop detector
/// (which ignores requests after seeing too many in a short window).
/// One request per peer per 60 s is enough — the sync coordinator already
/// manages the authoritative sync; this path is just a fallback.
pub(super) fn chain_tip_getblocks_rate_limit() -> &'static dashmap::DashMap<String, Instant> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, Instant>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// Tracks when each peer first fell ≥200 blocks behind our chain height.
/// Entry is removed when the peer catches up.  After ZOMBIE_TIMEOUT the peer
/// is kicked (writer channel closed + removed from registry) so it cannot
/// occupy a connection slot indefinitely.
pub(super) fn zombie_peer_tracker() -> &'static dashmap::DashMap<String, Instant> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, Instant>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// How long a peer may remain ≥200 blocks behind before being kicked.
pub(super) const ZOMBIE_TIMEOUT: Duration = Duration::from_secs(600);

/// Cached UTXO state hash received from a peer.
pub(super) struct PeerUtxoHashEntry {
    pub(super) hash: [u8; 32],
    pub(super) height: u64,
    pub(super) _utxo_count: usize,
    pub(super) received_at: Instant,
}

/// Global cache of peer UTXO state hashes for majority-based reconciliation.
/// Populated when peers respond to our GetUTXOStateHash broadcasts.
/// Entries older than 10 minutes are ignored during vote counting.
pub(super) fn peer_utxo_hash_cache() -> &'static dashmap::DashMap<String, PeerUtxoHashEntry> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, PeerUtxoHashEntry>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// Tracks consecutive UTXO consistency check rounds where we remain diverged.
/// Used for liveness-adjusted threshold: 2/3 → simple majority → plurality.
pub(super) fn utxo_divergence_rounds() -> &'static std::sync::atomic::AtomicU32 {
    static INSTANCE: std::sync::OnceLock<std::sync::atomic::AtomicU32> = std::sync::OnceLock::new();
    INSTANCE.get_or_init(|| std::sync::atomic::AtomicU32::new(0))
}

/// Per-peer accumulator for in-progress UTXOSetChunk transfers (from GetUTXOSet).
pub(super) fn utxo_set_chunk_buf() -> &'static dashmap::DashMap<String, Vec<crate::types::UTXO>> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, Vec<crate::types::UTXO>>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// Per-peer accumulator for in-progress UtxoReconciliationChunk transfers.
/// Value is (at_height, accumulated UTXOs).
pub(super) fn utxo_reconcil_chunk_buf(
) -> &'static dashmap::DashMap<String, (u64, Vec<crate::types::UTXO>)> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, (u64, Vec<crate::types::UTXO>)>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// Split a UTXO list into chunks that each serialise below 7 MiB.
/// Each UTXO is serialised with bincode to get an accurate byte count before
/// packing it into the current chunk.
pub(super) fn split_utxos_into_chunks(
    utxos: Vec<crate::types::UTXO>,
) -> Vec<Vec<crate::types::UTXO>> {
    const MAX_CHUNK_BYTES: usize = 7 * 1024 * 1024;
    let mut chunks: Vec<Vec<crate::types::UTXO>> = Vec::new();
    let mut current: Vec<crate::types::UTXO> = Vec::new();
    // 8 bytes = bincode Vec<T> length prefix
    let mut current_bytes: usize = 8;
    for utxo in utxos {
        let utxo_bytes = bincode::serialize(&utxo).map(|v| v.len()).unwrap_or(400);
        if current_bytes + utxo_bytes > MAX_CHUNK_BYTES && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 8;
        }
        current_bytes += utxo_bytes;
        current.push(utxo);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(Vec::new());
    }
    chunks
}

/// Max age for cached peer UTXO hashes (10 minutes = 2 sync check cycles).
pub(super) const UTXO_HASH_CACHE_TTL: Duration = Duration::from_secs(600);

/// After a V4 proof eviction fires for an outpoint, block further V4 evictions
/// on that same outpoint for this many seconds.  Prevents infinite eviction storms
/// when multiple nodes simultaneously hold valid V4 proofs for the same collateral.
pub(super) const V4_EVICTION_COOLDOWN_SECS: u64 = 60;

/// AV30: Per-peer tracking of incoming ForkAlert → GetBlocks → rejected-blocks cycles.
/// Stores (last_getblocks_sent: Instant, rejected_cycles: u32, window_start: Instant).
/// A "rejected cycle" = we sent GetBlocks in response to a ForkAlert and all returned
/// blocks were skipped/rejected.  After FORK_ALERT_BAN_THRESHOLD rejected cycles within
/// FORK_ALERT_WINDOW, the peer is treated as a fork-bombing attacker and banned.
pub(super) fn incoming_fork_alert_tracker(
) -> &'static dashmap::DashMap<String, (Instant, u32, Instant)> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, (Instant, u32, Instant)>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}
/// Minimum gap between GetBlocks responses to the same peer's ForkAlert (AV30).
pub(super) const FORK_ALERT_RESPONSE_COOLDOWN: Duration = Duration::from_secs(30);
/// Window over which rejected fork-alert cycles are counted (AV30).
pub(super) const FORK_ALERT_WINDOW: Duration = Duration::from_secs(300);
/// Rejected cycles within FORK_ALERT_WINDOW before recording a ban violation (AV30).
pub(super) const FORK_ALERT_BAN_THRESHOLD: u32 = 5;

/// Per-outpoint timestamp of the last accepted V4 proof eviction.
pub(super) fn v4_eviction_cooldown() -> &'static dashmap::DashMap<String, std::time::Instant> {
    static INSTANCE: std::sync::OnceLock<dashmap::DashMap<String, std::time::Instant>> =
        std::sync::OnceLock::new();
    INSTANCE.get_or_init(dashmap::DashMap::new)
}

/// Check a (count, window_start) sliding window and return true when the
/// threshold is crossed (resetting the counter on each crossing).
pub(super) async fn check_sliding_window(
    window: &Mutex<(u32, Instant)>,
    threshold: u32,
    window_secs: u64,
) -> bool {
    let mut w = window.lock().await;
    let now = Instant::now();
    if now.duration_since(w.1) > Duration::from_secs(window_secs) {
        *w = (1, now);
        false
    } else {
        w.0 += 1;
        if w.0 >= threshold {
            w.0 = 0;
            w.1 = now;
            true
        } else {
            false
        }
    }
}

/// Tracks outpoints that have had ≥ 3 unique IPs permanently banned for claiming them
/// without proof.  Once an outpoint is "contested", any new non-V4 claimant is rejected
/// immediately — before the expensive UTXO lookup — preventing ban-list exhaustion DoS.
/// Key = "<txid_hex>:<vout>", value = count of distinct IPs banned for this outpoint.
pub(super) fn contested_outpoints() -> &'static dashmap::DashMap<String, u32> {
    static MAP: std::sync::OnceLock<dashmap::DashMap<String, u32>> = std::sync::OnceLock::new();
    MAP.get_or_init(dashmap::DashMap::new)
}

/// Threshold: after this many distinct IPs are banned for the same outpoint, mark it
/// as contested and reject all future non-V4 claims without doing any UTXO work.
pub(super) const CONTESTED_OUTPOINT_THRESHOLD: u32 = 3;

/// Rate-limit a log/action to once per `cooldown_secs` per key.
/// Returns true and records the current time when the cooldown has elapsed.
pub(super) fn should_warn_now(
    map: &dashmap::DashMap<String, Instant>,
    key: &str,
    cooldown_secs: u64,
) -> bool {
    let fire = map
        .get(key)
        .map(|t| t.elapsed().as_secs() >= cooldown_secs)
        .unwrap_or(true);
    if fire {
        map.insert(key.to_string(), Instant::now());
    }
    fire
}

/// Sign a consensus vote (PREPARE or PRECOMMIT) with the node's Ed25519 key.
/// Returns an empty vec if no signing key is configured.
pub(super) fn sign_vote(
    consensus: &ConsensusEngine,
    block_hash: &[u8; 32],
    voter_id: &str,
    vote_type: &[u8],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    consensus
        .get_signing_key()
        .map(|k| {
            let mut msg = Vec::with_capacity(32 + voter_id.len() + vote_type.len());
            msg.extend_from_slice(block_hash);
            msg.extend_from_slice(voter_id.as_bytes());
            msg.extend_from_slice(vote_type);
            k.sign(&msg).to_bytes().to_vec()
        })
        .unwrap_or_default()
}

/// Spawn a background task that calls `blockchain.handle_fork(blocks, peer_ip)`.
/// On reward-hijacking errors the peer is permanently banned and marked incompatible.
pub(super) fn spawn_fork_resolution(
    blockchain: Arc<Blockchain>,
    blocks: Vec<Block>,
    peer_ip: String,
    banlist: Option<Arc<RwLock<IPBanlist>>>,
    peer_registry: Arc<PeerConnectionRegistry>,
    masternode_registry: Arc<MasternodeRegistry>,
) {
    tokio::spawn(async move {
        if let Err(e) = blockchain.handle_fork(blocks, peer_ip.clone()).await {
            warn!("Fork resolution failed: {}", e);
            if e.contains("unique reward recipient") || e.contains("reward-hijacking") {
                // Never permanently ban a whitelisted peer — reward mismatch with
                // a whitelisted peer indicates local registry divergence, not an attack.
                let is_whitelisted = peer_registry.is_whitelisted(&peer_ip).await;
                if is_whitelisted {
                    warn!(
                        "⚠️ Reward mismatch in reorg from WHITELISTED peer {} — \
                         likely local registry divergence, not banning. Error: {}",
                        peer_ip, e
                    );
                } else {
                    warn!(
                        "⚠️ Reorg from {} had invalid reward distribution — temp-banning 6h: {}",
                        peer_ip, e
                    );
                    if let Some(bl) = &banlist {
                        let bare = peer_ip.split(':').next().unwrap_or(&peer_ip);
                        if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
                            bl.write().await.add_temp_ban(
                                ip,
                                std::time::Duration::from_secs(6 * 3600),
                                &format!("Invalid reorg reward: {}", e),
                            );
                        }
                    }
                    peer_registry
                        .mark_incompatible(
                            &peer_ip,
                            &format!("Reward-hijacking reorg chain: {}", e),
                            false, // not permanent
                        )
                        .await;
                    let _ = masternode_registry
                        .suspend_from_consensus(
                            &peer_ip,
                            &format!("Reward-hijacking reorg chain: {}", e),
                        )
                        .await;
                }
            }
        }
    });
}

/// Probe a masternode's P2P port to verify it is publicly reachable.
///
/// Called when a masternode connects to us **inbound** (they initiated the TCP
/// connection, so we don't yet know whether their port accepts inbound connections).
/// We attempt a TCP connect to their announced P2P address with a short timeout.
///
/// On success: marks the node as publicly reachable (reward-eligible).
/// On failure: marks the node as not reachable and sends a `ConnectivityWarning`
///             back over the existing connection explaining the issue.
pub async fn probe_masternode_reachability(
    peer_ip: String,
    network: crate::NetworkType,
    registry: Arc<MasternodeRegistry>,
    peer_registry: Arc<PeerConnectionRegistry>,
) {
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    let port = network.default_p2p_port();
    let target = format!("{}:{}", peer_ip, port);

    debug!(
        "🔍 Probing reachability of masternode {} ({})",
        peer_ip, target
    );

    let probe_result = timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect(&target),
    )
    .await;

    let reachable = matches!(probe_result, Ok(Ok(_)));

    if reachable {
        debug!(
            "✅ Reachability probe succeeded for {} — bidirectional connectivity confirmed",
            peer_ip
        );
    } else {
        let reason = match &probe_result {
            Err(_) => "connection timed out".to_string(),
            Ok(Err(e)) => format!("connection refused or failed: {}", e),
            Ok(Ok(_)) => unreachable!(),
        };
        warn!(
            "🚫 Reachability probe FAILED for {} ({}): {} — excluded from block rewards",
            peer_ip, target, reason
        );

        // Send a warning to the peer so it appears in their logs
        let warning_msg = crate::network::message::NetworkMessage::ConnectivityWarning {
            message: format!(
                "Your node at {} is not publicly reachable on port {} ({}). \
                 Block rewards require full bidirectional connectivity. \
                 Please run your masternode on a VPS or dedicated server with a static public IP \
                 and ensure port {} is open for inbound TCP connections. \
                 Home connections behind NAT/firewall without UPnP or port forwarding \
                 cannot participate in block rewards.",
                peer_ip, port, reason, port
            ),
        };
        if let Err(e) = peer_registry.send_to_peer(&peer_ip, warning_msg).await {
            debug!(
                "Could not deliver ConnectivityWarning to {}: {}",
                peer_ip, e
            );
        }
    }

    registry.set_publicly_reachable(&peer_ip, reachable).await;
}
