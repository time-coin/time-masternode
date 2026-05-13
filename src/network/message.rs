use crate::block::types::Block;
use crate::types::{Hash256, MasternodeTier, OutPoint, Transaction, UTXOState, UTXO};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NetworkMessage {
    // Block sync
    GetGenesisHash,
    GenesisHashResponse(Hash256),
    GetBlockHeight,
    BlockHeightResponse(u64),
    /// Get chain tip info for fork detection (height + hash)
    GetChainTip,
    /// Chain tip response with height and hash for fork comparison
    ChainTipResponse {
        height: u64,
        hash: Hash256,
    },
    GetBlocks(u64, u64), // (start_height, end_height)
    BlocksResponse(Vec<Block>),
    // Genesis coordination
    RequestGenesis,             // Non-leader requests genesis from network
    GenesisAnnouncement(Block), // Leader announces genesis creation
    // First message must be handshake with magic bytes
    Handshake {
        magic: [u8; 4],
        protocol_version: u32,
        network: String,
        /// Number of git commits in the sender's build. Used to detect outdated nodes.
        /// Nodes with a lower count than ours are flagged as running old code.
        commit_count: u32,
    },
    // Acknowledgment for handshake and critical messages
    Ack {
        message_type: String,
    },
    TransactionBroadcast(Transaction),
    TransactionFinalized {
        txid: [u8; 32],
        tx: Transaction, // Include full transaction for nodes that don't have it yet
    },
    UTXOStateQuery(Vec<OutPoint>),
    UTXOStateResponse(Vec<(OutPoint, UTXOState)>),
    UTXOStateNotification(UTXOStateChange),
    UTXOStateUpdate {
        outpoint: OutPoint,
        state: UTXOState,
    },
    Subscribe(Subscription),
    Unsubscribe(String),
    BlockAnnouncement(Block),
    // Inventory-based block propagation (more efficient)
    BlockInventory(u64), // Just announce the height, peer can request if needed
    BlockRequest(u64),
    BlockResponse(Block),
    GetUTXOSet,
    UTXOSetResponse(Vec<UTXO>),
    GetUTXOStateHash,
    UTXOStateHashResponse {
        hash: [u8; 32],
        height: u64,
        utxo_count: usize,
    },
    MasternodeAnnouncement {
        address: String,
        reward_address: String,
        tier: MasternodeTier,
        public_key: VerifyingKey,
    },
    /// Announce masternode deregistration and collateral unlock.
    ///
    /// Analogous to Dash's `ProUpRevTx`: a signed gossip message that tells peers to
    /// release the collateral lock for `address` without requiring the owner to spend
    /// the UTXO.  The `signature` field is an Ed25519 signature over:
    ///   `"TIME_COLLATERAL_REVOKE:<address>:<txid_hex>:<vout>:<timestamp>"`
    /// using the same masternodeprivkey that signed the V4 `collateral_proof`.
    /// Legacy messages with an empty signature are accepted only from a direct
    /// (non-relayed) connection whose source IP matches `address`.
    MasternodeUnlock {
        address: String,
        collateral_outpoint: OutPoint,
        timestamp: u64,
        /// Ed25519 signature over the revoke proof message (64 bytes), or empty for
        /// legacy unsigned messages (accepted only from the node itself).
        #[serde(default)]
        signature: Vec<u8>,
    },
    GetMasternodes,
    MasternodesResponse(Vec<MasternodeAnnouncementData>),
    /// Notify network that a masternode went offline/inactive
    MasternodeInactive {
        address: String,
        timestamp: u64,
    },
    /// Request locked collateral data from peer
    GetLockedCollaterals,
    /// Response with locked collateral data
    LockedCollateralsResponse(Vec<LockedCollateralData>),
    Version {
        version: String,
        commit_date: String,
        commit_count: String,
        protocol_version: u32,
        network: String,
        listen_addr: String,
        timestamp: i64,
        capabilities: Vec<String>,
        wallet_address: Option<String>,
        genesis_hash: Option<String>,
    },
    Ping {
        nonce: u64,
        timestamp: i64,
        height: Option<u64>, // Phase 3: Advertise our height in pings
    },
    Pong {
        nonce: u64,
        timestamp: i64,
        height: Option<u64>, // Phase 3: Include height in pong responses
    },
    GetPendingTransactions,
    PendingTransactionsResponse(Vec<Transaction>),
    /// Request full mempool state from a peer on connect (pending + finalized, with fees)
    MempoolSyncRequest,
    /// Full mempool state response: each entry carries the transaction, its fee, and whether
    /// it has already been finalized by TimeVote consensus on the responding node.
    MempoolSyncResponse(Vec<MempoolSyncEntry>),
    // Peer exchange
    GetPeers,
    PeersResponse(Vec<String>), // List of peer addresses (IP:port) — legacy, kept for compat
    /// Load-aware peer exchange: recipients should prefer peers with lower connection_count
    PeerExchange(Vec<PeerExchangeEntry>),
    // Fork resolution
    GetBlockHash(u64),
    BlockHashResponse {
        height: u64,
        hash: Option<[u8; 32]>,
    },
    ConsensusQuery {
        height: u64,
        block_hash: [u8; 32],
    },
    ConsensusQueryResponse {
        agrees: bool,
        height: u64,
        their_hash: [u8; 32],
    },
    GetBlockRange {
        start_height: u64,
        end_height: u64,
    },
    BlockRangeResponse(Vec<Block>),
    // Fork alert - notify peer they're on wrong chain
    ForkAlert {
        your_height: u64,
        your_hash: [u8; 32],
        consensus_height: u64,
        consensus_hash: [u8; 32],
        consensus_peer_count: usize,
        message: String,
    },
    // TimeVote consensus voting - Protocol §7 & §8
    // DEPRECATED: Use TimeVoteRequest instead
    TransactionVoteRequest {
        txid: Hash256,
    },
    // DEPRECATED: Use TimeVoteResponse instead
    TransactionVoteResponse {
        txid: Hash256,
        preference: String, // "Accept" or "Reject"
    },
    // TimeVote Protocol - Signed vote request (Protocol §8.1)
    // FIX: Include optional TX data so validators can process immediately
    // without waiting for separate TransactionBroadcast to propagate
    TimeVoteRequest {
        txid: Hash256,
        tx_hash_commitment: Hash256,
        slot_index: u64,
        tx: Option<crate::types::Transaction>, // NEW: Include TX for validators who don't have it
    },
    // TimeVote Protocol - Signed vote response (Protocol §8.1)
    TimeVoteResponse {
        vote: crate::types::TimeVote,
    },
    // TimeVote broadcast - for disseminating votes to all peers
    TimeVoteBroadcast {
        vote: crate::types::TimeVote,
    },
    // TimeProof broadcast - for disseminating finality certificates (Protocol §8.2)
    TimeProofBroadcast {
        proof: crate::types::TimeProof,
    },
    // Legacy aliases for backward compatibility
    // DEPRECATED: Use TimeVoteRequest instead
    FinalityVoteRequest {
        txid: Hash256,
        slot_index: u64,
    },
    // DEPRECATED: Use TimeVoteResponse instead
    FinalityVoteResponse {
        vote: crate::types::FinalityVote, // Type alias to TimeVote
    },
    // DEPRECATED: Use TimeVoteBroadcast instead
    FinalityVoteBroadcast {
        vote: crate::types::FinalityVote, // Type alias to TimeVote
    },
    // TimeLock Block production messages
    TimeLockBlockProposal {
        block: Block,
    },
    TimeVotePrepare {
        block_hash: Hash256,
        voter_id: String,
        signature: Vec<u8>,
    },
    TimeVotePrecommit {
        block_hash: Hash256,
        voter_id: String,
        signature: Vec<u8>,
    },
    // Chain comparison for fork detection
    GetChainWork,
    ChainWorkResponse {
        height: u64,
        tip_hash: [u8; 32],
        cumulative_work: u128,
    },
    // Request chain work at specific height for fork resolution
    GetChainWorkAt(u64),
    ChainWorkAtResponse {
        height: u64,
        block_hash: [u8; 32],
        cumulative_work: u128,
    },
    // §7.6 Liveness Fallback Protocol Messages
    /// Broadcast when a transaction stalls in Sampling state
    LivenessAlert {
        alert: crate::types::LivenessAlert,
    },
    /// Deterministic leader's proposal for stalled transaction
    FinalityProposal {
        proposal: crate::types::FinalityProposal,
    },
    /// Vote on a fallback finality proposal
    FallbackVote {
        vote: crate::types::FallbackVote,
    },
    /// Gossip-based masternode status tracking
    MasternodeStatusGossip {
        reporter: String,                 // Who is reporting
        visible_masternodes: Vec<String>, // List of masternode IPs they can see
        timestamp: u64,
    },
    /// Companion to MasternodeStatusGossip — carries daemon start timestamps
    /// so that nodes not directly connected to a peer can still show real uptime.
    /// Old nodes that cannot deserialize this variant receive UnknownMessage and
    /// silently ignore it (wire.rs fallback path).
    MasternodeStartedAtGossip {
        /// (masternode_ip, daemon_started_at) for all masternodes the reporter knows about
        entries: Vec<(String, u64)>,
    },
    /// V2 masternode announcement with collateral verification
    MasternodeAnnouncementV2 {
        address: String,
        reward_address: String,
        tier: MasternodeTier,
        public_key: VerifyingKey,
        collateral_outpoint: Option<OutPoint>,
    },
    /// V3 masternode announcement with website-issued certificate
    MasternodeAnnouncementV3 {
        address: String,
        reward_address: String,
        tier: MasternodeTier,
        public_key: VerifyingKey,
        collateral_outpoint: Option<OutPoint>,
        /// Ed25519 signature from the authority over the masternode's public key (64 bytes)
        /// Empty/zeroed for free nodes without certificates (allowed when enforcement is off)
        certificate: Vec<u8>,
        /// Unix timestamp when the announcing node's daemon started (for remote uptime display)
        #[serde(default)]
        started_at: u64,
    },
    /// V4 masternode announcement with collateral ownership proof.
    ///
    /// Adds a cryptographic proof that the announcing node controls the private key
    /// of the collateral UTXO's output address.  This prevents outpoint squatting:
    /// a bad actor who gossips a collateral outpoint first cannot block the legitimate
    /// owner from registering, because only the owner can produce a valid signature.
    ///
    /// Proof message: `b"TIME Masternode:" + ip + b":" + txid_hex + b":" + vout`
    /// Signed with the Ed25519 key that controls the collateral UTXO's output address.
    /// Empty Vec means no proof (treated identically to V3 — first-claim wins).
    MasternodeAnnouncementV4 {
        address: String,
        reward_address: String,
        tier: MasternodeTier,
        public_key: VerifyingKey,
        collateral_outpoint: Option<OutPoint>,
        certificate: Vec<u8>,
        #[serde(default)]
        started_at: u64,
        /// Ed25519 signature over the collateral proof message (64 bytes), or empty.
        #[serde(default)]
        collateral_proof: Vec<u8>,
    },
    /// A payment request relayed between masternodes (24h TTL, signed by requester)
    PaymentRequestRelay(PaymentRequest),
    /// Requester cancelled their own pending payment request
    PaymentRequestCancelled {
        id: String,
        requester_address: String,
    },
    /// Payer responded to a payment request (accepted or declined)
    PaymentRequestResponse {
        id: String,
        requester_address: String,
        payer_address: String,
        accepted: bool,
        /// TXID of the payment transaction when accepted
        txid: Option<String>,
    },
    /// Payer opened/viewed a payment request (lets requester know it was seen)
    PaymentRequestViewed {
        id: String,
        requester_address: String,
        payer_address: String,
    },
    /// Gossip a new governance proposal to peers.
    GovernanceProposal(crate::governance::GovernanceProposal),
    /// Gossip a governance vote to peers.
    GovernanceVote(crate::governance::GovernanceVote),
    /// Request all active governance proposals and votes from a peer.
    GetGovernanceState,
    /// Response: all known proposals and votes.
    GovernanceStateResponse {
        proposals: Vec<crate::governance::GovernanceProposal>,
        votes: Vec<crate::governance::GovernanceVote>,
    },
    /// Sent by a masternode to a peer whose P2P port is not publicly reachable.
    /// Informs the operator they need full bidirectional connectivity (e.g. a VPS)
    /// to participate in block rewards.
    ConnectivityWarning {
        /// Human-readable explanation of the connectivity problem and how to fix it.
        message: String,
    },
    /// Request a full UTXO state snapshot for reconciliation. Sent when a node
    /// computes a different reward outcome than the producer, indicating its UTXO
    /// set has diverged. The responder sends all spendable UTXOs at `at_height`.
    RequestUtxoReconciliation {
        /// Block height at which the disagreement occurred.
        at_height: u64,
        /// Hash of the disputed block, so the responder can verify we're on the same chain.
        block_hash: [u8; 32],
    },
    /// Full UTXO snapshot response. Recipient should apply/merge these UTXOs,
    /// then re-verify reward computation. Node will not receive rewards until
    /// it can produce matching results (it won't appear in the bitmap until then).
    UtxoReconciliationResponse {
        at_height: u64,
        utxos: Vec<crate::types::UTXO>,
    },
    /// One page of a multi-frame UTXOSet transfer (response to GetUTXOSet).
    /// Sent when the full set exceeds the 8 MiB frame limit.  Receiver accumulates
    /// all chunks (index 0 … total-1) and then runs the normal diff/reconcile logic.
    UTXOSetChunk {
        index: u32,
        total: u32,
        utxos: Vec<crate::types::UTXO>,
    },
    /// One page of a multi-frame UTXO reconciliation transfer (response to
    /// RequestUtxoReconciliation).  Same chunking contract as UTXOSetChunk.
    UtxoReconciliationChunk {
        at_height: u64,
        index: u32,
        total: u32,
        utxos: Vec<crate::types::UTXO>,
    },
    /// Placeholder for messages from newer protocol versions that we can't parse
    UnknownMessage,
}

/// A single mempool entry carried in a `MempoolSyncResponse`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MempoolSyncEntry {
    pub tx: Transaction,
    /// Miner fee in satoshis (input_sum − output_sum)
    pub fee: u64,
    /// True when this transaction has already passed TimeVote finalization on the sending node
    pub is_finalized: bool,
}

/// An entry in a PeerExchange message — a peer address with its current connection load
/// and tier.  Recipients use tier to route connections up the pyramid (Free→Bronze→Silver→Gold)
/// and use connection_count to prefer less-loaded peers within each tier.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PeerExchangeEntry {
    /// IP address of the peer (no port — port is derived from NetworkType)
    pub address: String,
    /// Number of active connections this peer currently has (best-effort, may be slightly stale)
    pub connection_count: u16,
    /// True if this peer is a registered masternode
    pub is_masternode: bool,
    /// Masternode tier — drives pyramid routing (None = unregistered / regular peer)
    pub tier: Option<MasternodeTier>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UTXOStateChange {
    pub outpoint: OutPoint,
    pub new_state: UTXOState,
    pub timestamp: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Subscription {
    pub id: String,
    pub addresses: Vec<String>,
    pub outpoints: Vec<OutPoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MasternodeAnnouncementData {
    pub address: String,
    pub reward_address: String,
    pub tier: MasternodeTier,
    pub public_key: VerifyingKey,
    /// Collateral outpoint (None for legacy masternodes)
    pub collateral_outpoint: Option<OutPoint>,
    /// Timestamp when registered
    pub registered_at: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LockedCollateralData {
    pub outpoint: OutPoint,
    pub masternode_address: String,
    pub lock_height: u64,
    pub locked_at: u64,
    pub amount: u64,
}

/// A signed payment request relayed via the P2P network.
/// The requester signs the request with their Ed25519 key; masternodes validate
/// the signature before storing/relaying. Requests expire after 24 hours.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaymentRequest {
    /// Deterministic ID: hex(SHA-256(from_address || to_address || amount || timestamp))
    pub id: String,
    /// Address of the requester (who wants to be paid)
    pub from_address: String,
    /// Address of the payer (who should pay)
    pub to_address: String,
    /// Amount in smallest units (satoshis)
    pub amount: u64,
    /// Plaintext memo describing what the payment is for
    pub memo: String,
    /// Optional display name for the requester (e.g. merchant name)
    #[serde(default)]
    pub requester_name: String,
    /// Requester's Ed25519 public key as hex (enables encrypted memo when paying)
    pub pubkey_hex: String,
    /// Ed25519 signature as hex over (id || from_address || to_address || amount || memo || timestamp)
    pub signature_hex: String,
    /// Unix timestamp when the request was created
    pub timestamp: i64,
    /// Unix timestamp when the request expires (timestamp + 86400)
    pub expires: i64,
}

impl NetworkMessage {
    /// Get the message type name as a string (for logging/debugging)
    /// Note: Used in Phase 2 optimizations
    #[allow(dead_code)]
    pub fn message_type(&self) -> &'static str {
        match self {
            NetworkMessage::GetGenesisHash => "GetGenesisHash",
            NetworkMessage::GenesisHashResponse(_) => "GenesisHashResponse",
            NetworkMessage::GetBlockHeight => "GetBlockHeight",
            NetworkMessage::BlockHeightResponse(_) => "BlockHeightResponse",
            NetworkMessage::GetChainTip => "GetChainTip",
            NetworkMessage::ChainTipResponse { .. } => "ChainTipResponse",
            NetworkMessage::GetBlocks(_, _) => "GetBlocks",
            NetworkMessage::BlocksResponse(_) => "BlocksResponse",
            NetworkMessage::RequestGenesis => "RequestGenesis",
            NetworkMessage::GenesisAnnouncement(_) => "GenesisAnnouncement",
            NetworkMessage::Handshake { .. } => "Handshake",
            NetworkMessage::Ack { .. } => "Ack",
            NetworkMessage::TransactionBroadcast(_) => "TransactionBroadcast",
            NetworkMessage::TransactionFinalized { .. } => "TransactionFinalized",
            NetworkMessage::UTXOStateQuery(_) => "UTXOStateQuery",
            NetworkMessage::UTXOStateResponse(_) => "UTXOStateResponse",
            NetworkMessage::UTXOStateNotification(_) => "UTXOStateNotification",
            NetworkMessage::UTXOStateUpdate { .. } => "UTXOStateUpdate",
            NetworkMessage::Subscribe(_) => "Subscribe",
            NetworkMessage::Unsubscribe(_) => "Unsubscribe",
            NetworkMessage::BlockAnnouncement(_) => "BlockAnnouncement",
            NetworkMessage::BlockInventory(_) => "BlockInventory",
            NetworkMessage::BlockRequest(_) => "BlockRequest",
            NetworkMessage::BlockResponse(_) => "BlockResponse",
            NetworkMessage::GetUTXOSet => "GetUTXOSet",
            NetworkMessage::UTXOSetResponse(_) => "UTXOSetResponse",
            NetworkMessage::GetUTXOStateHash => "GetUTXOStateHash",
            NetworkMessage::UTXOStateHashResponse { .. } => "UTXOStateHashResponse",
            NetworkMessage::MasternodeAnnouncement { .. } => "MasternodeAnnouncement",
            NetworkMessage::MasternodeUnlock { .. } => "MasternodeUnlock",
            NetworkMessage::GetMasternodes => "GetMasternodes",
            NetworkMessage::MasternodesResponse(_) => "MasternodesResponse",
            NetworkMessage::MasternodeInactive { .. } => "MasternodeInactive",
            NetworkMessage::GetLockedCollaterals => "GetLockedCollaterals",
            NetworkMessage::LockedCollateralsResponse(_) => "LockedCollateralsResponse",
            NetworkMessage::Version { .. } => "Version",
            NetworkMessage::Ping { .. } => "Ping",
            NetworkMessage::Pong { .. } => "Pong",
            NetworkMessage::GetPendingTransactions => "GetPendingTransactions",
            NetworkMessage::PendingTransactionsResponse(_) => "PendingTransactionsResponse",
            NetworkMessage::MempoolSyncRequest => "MempoolSyncRequest",
            NetworkMessage::MempoolSyncResponse(_) => "MempoolSyncResponse",
            NetworkMessage::GetPeers => "GetPeers",
            NetworkMessage::PeersResponse(_) => "PeersResponse",
            NetworkMessage::GetBlockHash(_) => "GetBlockHash",
            NetworkMessage::BlockHashResponse { .. } => "BlockHashResponse",
            NetworkMessage::ConsensusQuery { .. } => "ConsensusQuery",
            NetworkMessage::ConsensusQueryResponse { .. } => "ConsensusQueryResponse",
            NetworkMessage::GetBlockRange { .. } => "GetBlockRange",
            NetworkMessage::BlockRangeResponse(_) => "BlockRangeResponse",
            NetworkMessage::TransactionVoteRequest { .. } => "TransactionVoteRequest",
            NetworkMessage::TransactionVoteResponse { .. } => "TransactionVoteResponse",
            NetworkMessage::TimeVoteRequest { .. } => "TimeVoteRequest",
            NetworkMessage::TimeVoteResponse { .. } => "TimeVoteResponse",
            NetworkMessage::TimeVoteBroadcast { .. } => "TimeVoteBroadcast",
            NetworkMessage::TimeProofBroadcast { .. } => "TimeProofBroadcast",
            NetworkMessage::FinalityVoteRequest { .. } => "FinalityVoteRequest",
            NetworkMessage::FinalityVoteResponse { .. } => "FinalityVoteResponse",
            NetworkMessage::FinalityVoteBroadcast { .. } => "FinalityVoteBroadcast",
            NetworkMessage::TimeLockBlockProposal { .. } => "TimeLockBlockProposal",
            NetworkMessage::TimeVotePrepare { .. } => "TimeVotePrepare",
            NetworkMessage::TimeVotePrecommit { .. } => "TimeVotePrecommit",
            NetworkMessage::GetChainWork => "GetChainWork",
            NetworkMessage::ChainWorkResponse { .. } => "ChainWorkResponse",
            NetworkMessage::GetChainWorkAt(_) => "GetChainWorkAt",
            NetworkMessage::ChainWorkAtResponse { .. } => "ChainWorkAtResponse",
            NetworkMessage::ForkAlert { .. } => "ForkAlert",
            NetworkMessage::LivenessAlert { .. } => "LivenessAlert",
            NetworkMessage::FinalityProposal { .. } => "FinalityProposal",
            NetworkMessage::FallbackVote { .. } => "FallbackVote",
            NetworkMessage::MasternodeStatusGossip { .. } => "MasternodeStatusGossip",
            NetworkMessage::MasternodeStartedAtGossip { .. } => "MasternodeStartedAtGossip",
            NetworkMessage::MasternodeAnnouncementV2 { .. } => "MasternodeAnnouncementV2",
            NetworkMessage::MasternodeAnnouncementV3 { .. } => "MasternodeAnnouncementV3",
            NetworkMessage::PaymentRequestRelay(_) => "PaymentRequestRelay",
            NetworkMessage::PaymentRequestCancelled { .. } => "PaymentRequestCancelled",
            NetworkMessage::PaymentRequestResponse { .. } => "PaymentRequestResponse",
            NetworkMessage::PaymentRequestViewed { .. } => "PaymentRequestViewed",
            NetworkMessage::UnknownMessage => "UnknownMessage",
            NetworkMessage::PeerExchange(_) => "PeerExchange",
            NetworkMessage::GovernanceProposal(_) => "GovernanceProposal",
            NetworkMessage::GovernanceVote(_) => "GovernanceVote",
            NetworkMessage::GetGovernanceState => "GetGovernanceState",
            NetworkMessage::GovernanceStateResponse { .. } => "GovernanceStateResponse",
            NetworkMessage::ConnectivityWarning { .. } => "ConnectivityWarning",
            NetworkMessage::MasternodeAnnouncementV4 { .. } => "MasternodeAnnouncementV4",
            NetworkMessage::RequestUtxoReconciliation { .. } => "RequestUtxoReconciliation",
            NetworkMessage::UtxoReconciliationResponse { .. } => "UtxoReconciliationResponse",
            NetworkMessage::UTXOSetChunk { .. } => "UTXOSetChunk",
            NetworkMessage::UtxoReconciliationChunk { .. } => "UtxoReconciliationChunk",
        }
    }

    /// Check if this is a critical message requiring acknowledgment
    /// Note: Used in Phase 2 message routing
    #[allow(dead_code)]
    pub fn requires_ack(&self) -> bool {
        matches!(
            self,
            NetworkMessage::Handshake { .. } | NetworkMessage::TransactionFinalized { .. }
        )
    }

    /// Check if this is a response message (not a request)
    /// Note: Used in Phase 2 message routing
    #[allow(dead_code)]
    pub fn is_response(&self) -> bool {
        matches!(
            self,
            NetworkMessage::GenesisHashResponse(_)
                | NetworkMessage::BlockHeightResponse(_)
                | NetworkMessage::ChainTipResponse { .. }
                | NetworkMessage::BlocksResponse(_)
                | NetworkMessage::Ack { .. }
                | NetworkMessage::UTXOStateResponse(_)
                | NetworkMessage::UTXOSetResponse(_)
                | NetworkMessage::UTXOStateHashResponse { .. }
                | NetworkMessage::MasternodesResponse(_)
                | NetworkMessage::PendingTransactionsResponse(_)
                | NetworkMessage::MempoolSyncResponse(_)
                | NetworkMessage::PeersResponse(_)
                | NetworkMessage::BlockHashResponse { .. }
                | NetworkMessage::ConsensusQueryResponse { .. }
                | NetworkMessage::BlockRangeResponse(_)
                | NetworkMessage::Pong { .. }
                | NetworkMessage::BlockResponse(_)
                | NetworkMessage::TransactionVoteResponse { .. }
                | NetworkMessage::FinalityVoteResponse { .. }
                | NetworkMessage::ChainWorkResponse { .. }
                | NetworkMessage::ChainWorkAtResponse { .. }
                | NetworkMessage::UTXOSetChunk { .. }
                | NetworkMessage::UtxoReconciliationChunk { .. }
        )
    }

    /// Check if this is a high priority message
    /// Note: Used in Phase 2 message routing
    #[allow(dead_code)]
    pub fn is_high_priority(&self) -> bool {
        matches!(
            self,
            NetworkMessage::Ping { .. } | NetworkMessage::Pong { .. }
        )
    }
}
