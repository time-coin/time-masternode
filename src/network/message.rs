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
    },
    // Acknowledgment for handshake and critical messages
    Ack {
        message_type: String,
    },
    TransactionBroadcast(Transaction),
    TransactionFinalized {
        txid: [u8; 32],
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
    /// Announce masternode deregistration and collateral unlock
    MasternodeUnlock {
        address: String,
        collateral_outpoint: OutPoint,
        timestamp: u64,
    },
    GetMasternodes,
    MasternodesResponse(Vec<MasternodeAnnouncementData>),
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
    // Peer exchange
    GetPeers,
    PeersResponse(Vec<String>), // List of peer addresses (IP:port)
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
    // timevote consensus voting
    TransactionVoteRequest {
        txid: Hash256,
    },
    TransactionVoteResponse {
        txid: Hash256,
        preference: String, // "Accept" or "Reject"
    },
    // Verifiable Finality Proofs (VFP) - Per Protocol §8
    FinalityVoteRequest {
        txid: Hash256,
        slot_index: u64,
    },
    FinalityVoteResponse {
        vote: crate::types::FinalityVote,
    },
    // Finality vote broadcast - for disseminating votes to all peers
    FinalityVoteBroadcast {
        vote: crate::types::FinalityVote,
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
            NetworkMessage::GetLockedCollaterals => "GetLockedCollaterals",
            NetworkMessage::LockedCollateralsResponse(_) => "LockedCollateralsResponse",
            NetworkMessage::Version { .. } => "Version",
            NetworkMessage::Ping { .. } => "Ping",
            NetworkMessage::Pong { .. } => "Pong",
            NetworkMessage::GetPendingTransactions => "GetPendingTransactions",
            NetworkMessage::PendingTransactionsResponse(_) => "PendingTransactionsResponse",
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
