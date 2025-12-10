use crate::block::types::Block;
use crate::types::{Hash256, MasternodeTier, OutPoint, Transaction, UTXOState, Vote, UTXO};
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
    TransactionBroadcast(Transaction),
    TransactionVoteRequest([u8; 32]),
    TransactionVote(Vote),
    TransactionFinalized {
        txid: [u8; 32],
        votes: u32,
    },
    TransactionRejected {
        txid: [u8; 32],
        reason: String,
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
    MasternodeAnnouncement {
        address: String,
        reward_address: String,
        tier: MasternodeTier,
        public_key: VerifyingKey,
    },
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
