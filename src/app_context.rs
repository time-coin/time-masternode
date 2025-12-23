use crate::avalanche_consensus::AvalancheConsensus;
use crate::blockchain::Blockchain;
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
#[allow(dead_code)]
pub struct AppContext {
    pub config: crate::config::Config,
    pub blockchain: Arc<Blockchain>,
    pub avalanche_consensus: Arc<AvalancheConsensus>,
    pub registry: Arc<MasternodeRegistry>,
    pub peer_manager: Arc<PeerManager>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub attestation_system: Arc<HeartbeatAttestationSystem>,
    pub connection_manager: Arc<ConnectionManager>,
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub peer_state: Arc<PeerStateManager>,
    pub wallet: WalletManager,
    pub masternode_info: Option<Masternode>,
}

impl AppContext {
    /// Create a minimal context for testing
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn test_context() -> Self {
        unimplemented!("Use AppBuilder for test setup")
    }
}
