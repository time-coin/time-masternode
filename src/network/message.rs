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
    GetBlocks(u64, u64), // (start_height, end_height)
    BlocksResponse(Vec<Block>),
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
    GetMasternodes,
    MasternodesResponse(Vec<MasternodeAnnouncementData>),
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
    },
    Pong {
        nonce: u64,
        timestamp: i64,
    },
    GetPendingTransactions,
    PendingTransactionsResponse(Vec<Transaction>),
    // Peer exchange
    GetPeers,
    PeersResponse(Vec<String>), // List of peer addresses (IP:port)
    // Heartbeat attestation
    HeartbeatBroadcast(crate::heartbeat_attestation::SignedHeartbeat),
    HeartbeatAttestation(crate::heartbeat_attestation::WitnessAttestation),
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
    // Avalanche consensus voting
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
            NetworkMessage::GetBlocks(_, _) => "GetBlocks",
            NetworkMessage::BlocksResponse(_) => "BlocksResponse",
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
            NetworkMessage::BlockRequest(_) => "BlockRequest",
            NetworkMessage::BlockResponse(_) => "BlockResponse",
            NetworkMessage::GetUTXOSet => "GetUTXOSet",
            NetworkMessage::UTXOSetResponse(_) => "UTXOSetResponse",
            NetworkMessage::GetUTXOStateHash => "GetUTXOStateHash",
            NetworkMessage::UTXOStateHashResponse { .. } => "UTXOStateHashResponse",
            NetworkMessage::MasternodeAnnouncement { .. } => "MasternodeAnnouncement",
            NetworkMessage::GetMasternodes => "GetMasternodes",
            NetworkMessage::MasternodesResponse(_) => "MasternodesResponse",
            NetworkMessage::Version { .. } => "Version",
            NetworkMessage::Ping { .. } => "Ping",
            NetworkMessage::Pong { .. } => "Pong",
            NetworkMessage::GetPendingTransactions => "GetPendingTransactions",
            NetworkMessage::PendingTransactionsResponse(_) => "PendingTransactionsResponse",
            NetworkMessage::GetPeers => "GetPeers",
            NetworkMessage::PeersResponse(_) => "PeersResponse",
            NetworkMessage::HeartbeatBroadcast(_) => "HeartbeatBroadcast",
            NetworkMessage::HeartbeatAttestation(_) => "HeartbeatAttestation",
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
