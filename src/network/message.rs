use crate::block::types::Block;
use crate::types::{OutPoint, Transaction, UTXOState, Vote, UTXO};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NetworkMessage {
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
    GetBlocks(u64, u64),
    BlocksResponse(Vec<Block>),
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
