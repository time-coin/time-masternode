use crate::bft_consensus::BFTConsensus;
use crate::blockchain::Blockchain;
use crate::consensus::ConsensusEngine;
use crate::heartbeat_attestation::HeartbeatAttestationSystem;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::network::peer_state::PeerStateManager;
use crate::peer_manager::PeerManager;
use crate::types::Masternode;
use crate::utxo_manager::UTXOStateManager;
use crate::wallet::WalletManager;
use std::sync::Arc;

/// Shared application context containing all major components
pub struct AppContext {
    pub config: crate::config::Config,
    pub blockchain: Arc<Blockchain>,
    pub consensus_engine: Arc<ConsensusEngine>,
    pub registry: Arc<MasternodeRegistry>,
    pub peer_manager: Arc<PeerManager>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub attestation_system: Arc<HeartbeatAttestationSystem>,
    pub connection_manager: Arc<ConnectionManager>,
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub peer_state: Arc<PeerStateManager>,
    pub wallet: WalletManager,
    pub masternode_info: Option<Masternode>,
    pub bft_consensus: Option<Arc<BFTConsensus>>,
}

impl AppContext {
    /// Create a minimal context for testing
    #[cfg(test)]
    pub fn test_context() -> Self {
        unimplemented!("Use AppBuilder for test setup")
    }
}
