//! Peer Connection Registry
//! Manages active peer connections and message routing.

#![allow(dead_code)]

mod admission;
mod chain_state;
mod connections;
mod discovery;
mod resources;
mod types;
mod writers;

pub use types::{PeerWriterTx, SharedPeerWriter};

/// Chain tip reported by a peer: (height, block_hash).
pub type ChainTip = types::ChainTip;

use crate::consensus::ConsensusEngine;
use crate::network::connection_manager::ConnectionManager;
use crate::network::message::NetworkMessage;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use tokio::sync::{broadcast, mpsc, RwLock};

use types::{IncompatiblePeerInfo, PendingPingMap, ResponseSender};

pub struct PeerConnectionRegistry {
    /// Authoritative connection lifecycle tracker (admission + direction + counts).
    connection_manager: OnceLock<Arc<ConnectionManager>>,
    // Map of peer IP to their write channel (sends pre-serialized frame bytes to I/O task)
    peer_writers: Arc<RwLock<HashMap<String, PeerWriterTx>>>,
    // Map of peer IP to their reported blockchain height
    peer_heights: Arc<RwLock<HashMap<String, u64>>>,
    // Map of peer IP to their latest ping RTT in seconds
    peer_ping_times: Arc<RwLock<HashMap<String, f64>>>,
    // Map of peer IP to pending ping send times (nonce -> sent_at) for RTT calculation
    pending_pings: Arc<RwLock<PendingPingMap>>,
    // Map of peer IP to their reported software commit count (from handshake)
    peer_commit_counts: Arc<RwLock<HashMap<String, u32>>>,
    // Map of peer IP to their chain tip (height + hash)
    peer_chain_tips: Arc<RwLock<HashMap<String, ChainTip>>>,
    // Pending responses for request/response pattern
    pending_responses: Arc<RwLock<HashMap<String, Vec<ResponseSender>>>>,
    // TimeLock consensus resources (shared from server)
    timelock_consensus: Arc<RwLock<Option<Arc<ConsensusEngine>>>>,
    timelock_block_cache: Arc<RwLock<Option<Arc<crate::network::block_cache::BlockCache>>>>,
    timelock_broadcast: Arc<RwLock<Option<broadcast::Sender<NetworkMessage>>>>,
    // WebSocket transaction event sender for real-time wallet notifications
    ws_tx_event_sender:
        Arc<RwLock<Option<broadcast::Sender<crate::rpc::websocket::TransactionEvent>>>>,
    // Banlist reference for checking whitelist status
    banlist: Arc<RwLock<Option<Arc<RwLock<crate::network::banlist::IPBanlist>>>>>,
    // Discovered peer candidates from peer exchange
    discovered_peers: Arc<RwLock<HashSet<String>>>,
    // Peers on incompatible chains (different hash calculation)
    // Maps peer IP -> (marked_at_timestamp, reason, is_permanent)
    // Permanent incompatibility (genesis mismatch) is never rechecked
    // Temporary incompatibility (hash mismatch) is rechecked after timeout
    incompatible_peers: Arc<RwLock<HashMap<String, IncompatiblePeerInfo>>>,
    // Persistent fork error counter per peer (tracks errors across multiple block requests)
    // Maps peer IP -> error count (resets on successful block add)
    fork_error_counts: DashMap<String, u32>,
    // Notified when any peer's chain tip is updated (for event-driven consensus checks)
    chain_tip_updated: Arc<tokio::sync::Notify>,
    // Cached result of get_compatible_peers() to avoid repeated lock acquisitions
    compatible_peers_cache: Arc<RwLock<(Vec<String>, std::time::Instant)>>,
    // Reported connection counts from peer exchange — used for load-aware routing
    peer_load: DashMap<String, u16>,
    // Peers whose genesis hash has been positively confirmed (same chain as us).
    // Only peers in this set are used for block sync.
    // Peers fail into incompatible_peers; unverified peers are verified on first chain tip.
    genesis_confirmed_peers: Arc<RwLock<HashSet<String>>>,
    // Peers currently undergoing genesis verification.
    // Prevents multiple concurrent GetBlockHash(0) requests to the same peer.
    pending_genesis_checks: Arc<dashmap::DashSet<String>>,
    // Tracks when we last attempted a genesis check for each peer (IP → Instant).
    // After a timeout we don't retry for GENESIS_CHECK_COOLDOWN_SECS to avoid
    // permanently flooding old-code nodes that never respond to GetBlockHash(0).
    genesis_check_last_attempt: Arc<dashmap::DashMap<String, std::time::Instant>>,
    // Time-stamped chain tip evidence from any peer, kept for 5 minutes past disconnect.
    // Prevents the minority-fork trap where majority-chain peers disconnect immediately
    // after detecting the fork — erasing their evidence before compare_chain_with_peers()
    // can accumulate the MIN_PEERS_FOR_FORK_SWITCH quorum.
    #[allow(clippy::type_complexity)]
    recent_chain_tip_cache: Arc<RwLock<HashMap<String, (u64, [u8; 32], std::time::Instant)>>>,
    // Node-wide dedup filter for vote relay.  Every unique (block_hash, voter_id, vote_type)
    // triple is relayed exactly once regardless of how many peers forward the same vote.
    // Without this, two peers bouncing the same vote message back and forth creates an
    // O(N²) relay loop that saturates the rate limiter (AV-relay-loop).
    pub seen_votes: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    // Node-wide dedup filter for TransactionFinalized relay.  Each unique txid is gossiped
    // exactly once per rotation window regardless of how many peers send it.
    // Without this, the already-finalized re-gossip path creates the same O(N²) storm.
    pub seen_tx_finalized: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    /// Operator messages received from peers: (timestamp_secs, from_addr, message_text).
    /// Capped at 50 entries (oldest dropped when full). Shared with the RPC handler so
    /// the dashboard can poll `getoperatormessages` without a separate storage system.
    pub operator_messages: Arc<std::sync::Mutex<std::collections::VecDeque<(u64, String, String)>>>,
    /// Pending relay storage ack listeners, keyed by msg_id.
    /// `sendmessage` RPC registers a channel here; message handler sends on it when MsgRelayAck arrives.
    pub pending_relay_acks: Arc<DashMap<[u8; 32], mpsc::UnboundedSender<Vec<u8>>>>,
    /// Pending message fetch listeners, keyed by recipient_addr_hash.
    /// `getmessages` RPC registers a channel here; handler sends each envelope byte vec on it.
    pub pending_msg_envelopes: Arc<DashMap<[u8; 32], mpsc::UnboundedSender<Vec<u8>>>>,
    /// Pending pubkey query listeners, keyed by address_hash (SHA-256 of TIME1 address).
    pub pending_pubkey_queries: Arc<DashMap<[u8; 32], mpsc::UnboundedSender<[u8; 32]>>>,
}

impl PeerConnectionRegistry {
    pub fn new() -> Self {
        Self {
            connection_manager: OnceLock::new(),
            peer_writers: Arc::new(RwLock::new(HashMap::new())),
            peer_heights: Arc::new(RwLock::new(HashMap::new())),
            peer_ping_times: Arc::new(RwLock::new(HashMap::new())),
            pending_pings: Arc::new(RwLock::new(HashMap::new())),
            peer_commit_counts: Arc::new(RwLock::new(HashMap::new())),
            peer_chain_tips: Arc::new(RwLock::new(HashMap::new())),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            timelock_consensus: Arc::new(RwLock::new(None)),
            timelock_block_cache: Arc::new(RwLock::new(None)),
            timelock_broadcast: Arc::new(RwLock::new(None)),
            ws_tx_event_sender: Arc::new(RwLock::new(None)),
            banlist: Arc::new(RwLock::new(None)),
            discovered_peers: Arc::new(RwLock::new(HashSet::new())),
            incompatible_peers: Arc::new(RwLock::new(HashMap::new())),
            fork_error_counts: DashMap::new(),
            chain_tip_updated: Arc::new(tokio::sync::Notify::new()),
            compatible_peers_cache: Arc::new(RwLock::new((Vec::new(), std::time::Instant::now()))),
            peer_load: DashMap::new(),
            genesis_confirmed_peers: Arc::new(RwLock::new(HashSet::new())),
            pending_genesis_checks: Arc::new(dashmap::DashSet::new()),
            genesis_check_last_attempt: Arc::new(dashmap::DashMap::new()),
            recent_chain_tip_cache: Arc::new(RwLock::new(HashMap::new())),
            seen_votes: Arc::new(crate::network::dedup_filter::DeduplicationFilter::new(
                std::time::Duration::from_secs(300), // 5-min rotation matches one block slot
            )),
            seen_tx_finalized: Arc::new(crate::network::dedup_filter::DeduplicationFilter::new(
                std::time::Duration::from_secs(300), // same 5-min window as votes
            )),
            operator_messages: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            pending_relay_acks: Arc::new(DashMap::new()),
            pending_msg_envelopes: Arc::new(DashMap::new()),
            pending_pubkey_queries: Arc::new(DashMap::new()),
        }
    }
}

impl Default for PeerConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
