pub mod blacklist;
pub mod client;
pub mod connection_manager;
pub mod connection_state;
pub mod dedup_filter;
pub mod message;
pub mod peer_connection; // NEW: Unified peer connection
pub mod peer_connection_registry;
pub mod peer_discovery;
pub mod peer_state; // NEW: Unified peer state management
pub mod rate_limiter;
pub mod secure_transport;
pub mod server;
pub mod signed_message;
pub mod state_sync; // PHASE 3: Network state synchronization
pub mod sync_coordinator; // PHASE 3 PART 2: Synchronization coordinator
pub mod tls;
