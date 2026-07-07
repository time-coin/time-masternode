use super::common::OperatorMessages;
use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::banlist::IPBanlist;
use crate::network::dedup_filter::DeduplicationFilter;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::utxo_manager::UTXOStateManager;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// Context containing all dependencies needed for message handling
pub struct MessageContext {
    pub blockchain: Arc<Blockchain>,
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub masternode_registry: Arc<MasternodeRegistry>,
    pub consensus: Option<Arc<ConsensusEngine>>,
    pub block_cache: Option<Arc<crate::network::block_cache::BlockCache>>,
    pub broadcast_tx: Option<broadcast::Sender<NetworkMessage>>,
    // Extended context for full message handling
    pub utxo_manager: Option<Arc<UTXOStateManager>>,
    pub peer_manager: Option<Arc<PeerManager>>,
    pub seen_blocks: Option<Arc<DeduplicationFilter>>,
    pub seen_transactions: Option<Arc<DeduplicationFilter>>,
    pub seen_tx_finalized: Option<Arc<crate::network::dedup_filter::DeduplicationFilter>>,
    pub seen_utxo_locks: Option<Arc<crate::network::dedup_filter::DeduplicationFilter>>,
    // Node-wide vote relay dedup — prevents relay loops between peers
    pub seen_votes: Option<Arc<crate::network::dedup_filter::DeduplicationFilter>>,
    // Node identity for voting
    pub node_masternode_address: Option<String>,
    // Banlist for rejecting messages from banned peers
    pub banlist: Option<Arc<RwLock<IPBanlist>>>,
    // AI System for recording events and making intelligent decisions
    pub ai_system: Option<Arc<crate::ai::AISystem>>,
    // WebSocket transaction event sender for real-time wallet notifications
    pub tx_event_sender: Option<broadcast::Sender<crate::rpc::websocket::TransactionEvent>>,
    // Per-peer clock drift tracker
    pub drift_tracker: Option<Arc<tokio::sync::Mutex<crate::time_sync::PeerDriftTracker>>>,
    // Shared operator message inbox (timestamp, from, message). Capped at 50 entries.
    pub operator_messages: OperatorMessages,
    // Relay store for Silver/Gold nodes — stores/retrieves encrypted message envelopes.
    pub relay_store: Option<Arc<crate::messaging::relay::RelayStore>>,
    // Signing key used to sign relay delivery events and storage acks.
    pub relay_signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    // Contacts book — persists pubkeys across restarts and acts as fallback for pubkey queries.
    pub contacts_book: Option<Arc<crate::messaging::contacts::ContactsBook>>,
}

impl MessageContext {
    /// Create a minimal context with only required fields
    pub fn minimal(
        blockchain: Arc<Blockchain>,
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Self {
        Self {
            blockchain,
            peer_registry,
            masternode_registry,
            consensus: None,
            block_cache: None,
            broadcast_tx: None,
            utxo_manager: None,
            peer_manager: None,
            seen_blocks: None,
            seen_transactions: None,
            seen_tx_finalized: None,
            seen_utxo_locks: None,
            seen_votes: None,
            node_masternode_address: None,
            banlist: None,
            ai_system: None,
            tx_event_sender: None,
            drift_tracker: None,
            operator_messages: None,
            relay_store: None,
            relay_signing_key: None,
            contacts_book: None,
        }
    }

    /// Create context with consensus resources for transaction/block handling
    pub fn with_consensus(
        blockchain: Arc<Blockchain>,
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
        consensus: Arc<ConsensusEngine>,
        block_cache: Arc<crate::network::block_cache::BlockCache>,
        broadcast_tx: broadcast::Sender<NetworkMessage>,
        node_masternode_address: Option<String>,
    ) -> Self {
        Self {
            blockchain,
            peer_registry,
            masternode_registry,
            consensus: Some(consensus),
            block_cache: Some(block_cache),
            broadcast_tx: Some(broadcast_tx),
            utxo_manager: None,
            peer_manager: None,
            seen_blocks: None,
            seen_transactions: None,
            seen_tx_finalized: None,
            seen_utxo_locks: None,
            seen_votes: None,
            node_masternode_address,
            banlist: None,
            ai_system: None,
            tx_event_sender: None,
            drift_tracker: None,
            operator_messages: None,
            relay_store: None,
            relay_signing_key: None,
            contacts_book: None,
        }
    }

    /// Create context and automatically fetch consensus resources from peer registry
    /// This is the preferred method for creating MessageContext as it ensures
    /// consensus engine is available for block/vote handling
    pub async fn from_registry(
        blockchain: Arc<Blockchain>,
        peer_registry: Arc<PeerConnectionRegistry>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Self {
        // Fetch consensus resources from peer registry
        let (consensus, block_cache, broadcast_tx) = peer_registry.get_timelock_resources().await;
        // Get local masternode address for voting identity
        let node_masternode_address = masternode_registry.get_local_address().await;
        // Get AI system from blockchain if available
        let ai_system = blockchain.ai_system().cloned();

        // Populate utxo_manager from consensus engine if available
        let utxo_manager = consensus.as_ref().map(|c| Arc::clone(&c.utxo_manager));

        // Fetch WebSocket tx event sender from peer registry
        let tx_event_sender = peer_registry.get_tx_event_sender().await;
        let seen_votes = Arc::clone(&peer_registry.seen_votes);
        let seen_tx_finalized = Arc::clone(&peer_registry.seen_tx_finalized);
        let operator_messages = Arc::clone(&peer_registry.operator_messages);

        Self {
            blockchain,
            peer_registry,
            masternode_registry,
            consensus,
            block_cache,
            broadcast_tx,
            utxo_manager,
            peer_manager: None,
            seen_blocks: None,
            seen_transactions: None,
            seen_tx_finalized: Some(seen_tx_finalized),
            seen_utxo_locks: None,
            seen_votes: Some(seen_votes),
            node_masternode_address,
            banlist: None,
            ai_system,
            tx_event_sender,
            drift_tracker: None,
            operator_messages: Some(operator_messages),
            relay_store: None,
            relay_signing_key: None,
            contacts_book: None,
        }
    }

    /// Set the relay store and signing key (Silver/Gold nodes only).
    pub fn with_relay_store(
        mut self,
        relay_store: Arc<crate::messaging::relay::RelayStore>,
        signing_key: Arc<ed25519_dalek::SigningKey>,
    ) -> Self {
        self.relay_store = Some(relay_store);
        self.relay_signing_key = Some(signing_key);
        self
    }

    /// Set the contacts book for persistent pubkey lookups across restarts.
    pub fn with_contacts_book(
        mut self,
        contacts_book: Arc<crate::messaging::contacts::ContactsBook>,
    ) -> Self {
        self.contacts_book = Some(contacts_book);
        self
    }

    /// Set the node's masternode address for voting identity
    pub fn with_node_address(mut self, address: Option<String>) -> Self {
        self.node_masternode_address = address;
        self
    }

    /// Set the banlist for rejecting messages from banned peers
    pub fn with_banlist(mut self, banlist: Arc<RwLock<IPBanlist>>) -> Self {
        self.banlist = Some(banlist);
        self
    }

    /// Set the AI system for intelligent event recording and decision making
    pub fn with_ai_system(mut self, ai_system: Arc<crate::ai::AISystem>) -> Self {
        self.ai_system = Some(ai_system);
        self
    }

    /// Override the vote relay dedup filter (used in tests; production path gets it from peer_registry)
    pub fn with_seen_votes(
        mut self,
        seen_votes: Arc<crate::network::dedup_filter::DeduplicationFilter>,
    ) -> Self {
        self.seen_votes = Some(seen_votes);
        self
    }
}
