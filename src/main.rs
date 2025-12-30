mod address;
mod avalanche;
mod block;
mod blockchain;
mod config;
mod consensus;
mod crypto;
mod error;
mod finality_proof;
mod heartbeat_attestation;
mod masternode_registry;
mod network;
mod network_type;
mod peer_manager;
mod rpc;
mod shutdown;
mod state_notifier;
mod storage;
mod time_sync;
mod transaction_pool;
mod tsdc;
mod types;
mod utxo_manager;
mod wallet;

use blockchain::Blockchain;
use chrono::Timelike;
use clap::Parser;
use config::Config;
use consensus::ConsensusEngine;
use heartbeat_attestation::HeartbeatAttestationSystem;
use masternode_registry::MasternodeRegistry;
use network::connection_manager::ConnectionManager;
use network::message::NetworkMessage;
use network::peer_connection::PeerStateManager;
use network::peer_connection_registry::PeerConnectionRegistry;
use network::server::NetworkServer;
use network_type::NetworkType;
use peer_manager::PeerManager;
use rpc::server::RpcServer;
use shutdown::ShutdownManager;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage::{InMemoryUtxoStorage, UtxoStorage};
use time_sync::TimeSync;
use tsdc::TSCDConsensus;
use types::*;
use utxo_manager::UTXOStateManager;
use wallet::WalletManager;

#[derive(Parser, Debug)]
#[command(name = "timed")]
#[command(about = "TIME Coin Protocol Daemon", long_about = None)]
struct Args {
    #[arg(short, long, default_value = "config.toml")]
    config: String,

    #[arg(long)]
    listen_addr: Option<String>,

    #[arg(long)]
    masternode: bool,

    #[arg(short, long)]
    verbose: bool,

    /// Run demo transaction on startup
    #[arg(long)]
    demo: bool,

    #[arg(long)]
    generate_config: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Print hostname at startup BEFORE any logging
    if let Ok(hostname) = hostname::get() {
        if let Ok(hostname_str) = hostname.into_string() {
            let short_name = hostname_str.split('.').next().unwrap_or(&hostname_str);
            eprintln!("\n╔═══════════════════════════════════════════╗");
            eprintln!("║  🖥️  NODE: {:<30} ║", short_name);
            eprintln!("╚═══════════════════════════════════════════╝\n");
        }
    }

    // Determine network type from config file or default to testnet
    let network_type = if let Ok(cfg) = Config::load_from_file(&args.config) {
        cfg.node.network_type()
    } else {
        NetworkType::Testnet
    };

    if args.generate_config {
        let config = Config::default();
        match config.save_to_file(&args.config) {
            Ok(_) => {
                println!("✅ Generated default config at: {}", args.config);
                return;
            }
            Err(e) => {
                eprintln!("❌ Failed to generate config: {}", e);
                std::process::exit(1);
            }
        }
    }

    // Load or create config with network-specific data directory
    let config = match Config::load_or_create(&args.config, &network_type) {
        Ok(cfg) => {
            println!("✓ Loaded configuration from {}", args.config);
            cfg
        }
        Err(e) => {
            eprintln!("❌ Failed to load config: {}", e);
            std::process::exit(1);
        }
    };

    setup_logging(&config.logging, args.verbose);

    let mut shutdown_manager = ShutdownManager::new();
    let shutdown_token = shutdown_manager.token();

    let network_type = config.node.network_type();
    let p2p_addr = config.network.full_listen_address(&network_type);
    let rpc_addr = config.rpc.full_listen_address(&network_type);

    // Get version info
    let version = env!("CARGO_PKG_VERSION");
    let git_hash = option_env!("GIT_HASH").unwrap_or("unknown");
    let build_date = option_env!("BUILD_DATE").unwrap_or("unknown");

    println!("\n🚀 TIME Coin Protocol Daemon v{} ({})", version, git_hash);
    println!("  └─ Build: {}", build_date);
    println!("═══════════════════════════════════════════════════════");
    println!();
    println!("📡 Network: {:?}", network_type);
    println!("  └─ Magic Bytes: {:?}", network_type.magic_bytes());
    println!("  └─ Address Prefix: {}", network_type.address_prefix());
    println!("  └─ Data Dir: {}", config.storage.data_dir);
    println!();

    // Initialize wallet manager
    let wallet_manager = WalletManager::new(config.storage.data_dir.clone());
    let wallet = match wallet_manager.get_or_create_wallet(network_type) {
        Ok(w) => {
            println!("✓ Wallet initialized");
            println!("  └─ Address: {}", w.address());
            println!("  └─ File: {}", wallet_manager.default_wallet_path());
            w
        }
        Err(e) => {
            eprintln!("❌ Failed to initialize wallet: {}", e);
            std::process::exit(1);
        }
    };
    println!();

    // Initialize masternode info for later registration
    let masternode_info: Option<types::Masternode> = if config.masternode.enabled {
        // Always use the wallet's address (auto-generated per node)
        let wallet_address = wallet.address().to_string();

        let tier = match config.masternode.tier.to_lowercase().as_str() {
            "free" => types::MasternodeTier::Free,
            "bronze" => types::MasternodeTier::Bronze,
            "silver" => types::MasternodeTier::Silver,
            "gold" => types::MasternodeTier::Gold,
            _ => {
                eprintln!(
                    "❌ Error: Invalid masternode tier '{}' (must be free/bronze/silver/gold)",
                    config.masternode.tier
                );
                std::process::exit(1);
            }
        };

        // Get external address and extract IP only (no port) for consistent masternode identification
        let full_address = config.network.full_external_address(&network_type);
        let ip_only = full_address
            .split(':')
            .next()
            .unwrap_or(&full_address)
            .to_string();

        let masternode = types::Masternode {
            address: ip_only,
            wallet_address: wallet_address.clone(),
            collateral: tier.collateral(),
            tier,
            public_key: *wallet.public_key(),
            registered_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        println!("✓ Running as {:?} masternode", tier);
        println!("  └─ Wallet: {}", wallet_address);
        println!("  └─ Collateral: {} TIME", tier.collateral());
        Some(masternode)
    } else {
        println!("⚠ No masternode configured - node will run in observer mode");
        println!("  To enable: Set masternode.enabled = true in config.toml");
        None
    };

    let storage: Arc<dyn UtxoStorage> = match config.storage.backend.as_str() {
        "memory" => {
            println!("✓ Using in-memory storage (testing mode)");
            Arc::new(InMemoryUtxoStorage::new())
        }
        "sled" => {
            println!("✓ Using Sled persistent storage");
            let db_dir = format!("{}/db", config.storage.data_dir);
            println!("  └─ Data directory: {}", db_dir);
            // Create db directory if it doesn't exist
            if let Err(e) = std::fs::create_dir_all(&db_dir) {
                println!("  ⚠ Failed to create db directory: {}", e);
            }
            match storage::SledUtxoStorage::new(&db_dir) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    println!("  ⚠ Sled failed: {}", e);
                    println!("  └─ Falling back to in-memory storage");
                    Arc::new(InMemoryUtxoStorage::new())
                }
            }
        }
        _ => {
            println!(
                "  ⚠ Unknown backend '{}', using in-memory",
                config.storage.backend
            );
            Arc::new(InMemoryUtxoStorage::new())
        }
    };

    // Helper function to calculate appropriate cache size based on available memory
    fn calculate_cache_size() -> u64 {
        use sysinfo::{MemoryRefreshKind, RefreshKind, System};
        let sys = System::new_with_specifics(
            RefreshKind::new().with_memory(MemoryRefreshKind::everything()),
        );
        let available_memory = sys.available_memory();

        // Use 10% of available memory per database, cap at 256MB each
        let cache_size = std::cmp::min(available_memory / 10, 256 * 1024 * 1024);

        tracing::info!(
            cache_mb = cache_size / (1024 * 1024),
            available_mb = available_memory / (1024 * 1024),
            "Configuring sled cache"
        );

        cache_size
    }

    let cache_size = calculate_cache_size();

    // Initialize block storage
    let db_dir = format!("{}/db", config.storage.data_dir);
    let block_storage_path = format!("{}/blocks", db_dir);
    let block_storage = match sled::Config::new()
        .path(&block_storage_path)
        .cache_capacity(cache_size)
        .open()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ Failed to initialize block storage: {}", e);
            std::process::exit(1);
        }
    };

    let utxo_mgr = Arc::new(UTXOStateManager::new_with_storage(storage));

    // Initialize peer manager
    let peer_db = Arc::new(
        sled::Config::new()
            .path(format!("{}/peers", db_dir))
            .cache_capacity(cache_size)
            .open()
            .map_err(|e| format!("Failed to open peer database: {}", e))
            .unwrap(),
    );
    let peer_manager = Arc::new(PeerManager::new(
        peer_db,
        config.network.clone(),
        network_type,
    ));

    // Initialize masternode registry
    let registry_db_path = format!("{}/registry", db_dir);
    let registry_db = Arc::new(
        match sled::Config::new()
            .path(&registry_db_path)
            .cache_capacity(cache_size)
            .open()
        {
            Ok(db) => db,
            Err(e) => {
                eprintln!("❌ Failed to open registry database: {}", e);
                std::process::exit(1);
            }
        },
    );

    println!("🔍 Initializing peer manager...");
    if let Err(e) = peer_manager.initialize().await {
        eprintln!("⚠️ Peer manager initialization warning: {}", e);
    }
    let registry = Arc::new(MasternodeRegistry::new(registry_db.clone(), network_type));
    registry.set_peer_manager(peer_manager.clone()).await;
    println!("  ✅ Peer manager initialized");
    println!();

    // Initialize heartbeat attestation system
    let attestation_system = Arc::new(HeartbeatAttestationSystem::new());
    println!("  ✅ Heartbeat attestation system initialized");
    println!();

    println!("✓ Ready to process transactions\n");

    // Initialize ConsensusEngine
    let consensus_engine = Arc::new(ConsensusEngine::new(vec![], utxo_mgr.clone()));
    tracing::info!("✓ Consensus engine initialized");

    // Initialize TSDC consensus engine with masternode registry
    let tsdc_consensus = Arc::new(TSCDConsensus::with_masternode_registry(
        Default::default(),
        registry.clone(),
    ));
    tracing::info!("✓ TSDC consensus engine initialized");

    // Initialize blockchain
    let blockchain = Arc::new(Blockchain::new(
        block_storage,
        consensus_engine.clone(),
        registry.clone(),
        utxo_mgr.clone(),
        network_type,
    ));

    println!("✓ Blockchain initialized");
    println!();

    // Validate chain time on startup
    match blockchain.validate_chain_time().await {
        Ok(()) => {
            tracing::info!("✅ Chain time validation passed");
        }
        Err(e) => {
            tracing::error!("❌ Chain time validation failed: {}", e);
            tracing::error!("❌ Network is ahead of schedule - this indicates a consensus bug");
            tracing::error!(
                "❌ Manual intervention required: see analysis/CATCHUP_CONSENSUS_FIX.md"
            );
            // Don't panic - allow node to participate in network but log the issue
        }
    }

    // Create shared peer connection registry for both client and server
    let peer_connection_registry = Arc::new(PeerConnectionRegistry::new());

    // Create unified peer state manager for connection tracking
    let peer_state = Arc::new(PeerStateManager::new());
    let connection_manager = Arc::new(ConnectionManager::new());

    // Set peer registry on blockchain for request/response queries
    blockchain
        .set_peer_registry(peer_connection_registry.clone())
        .await;

    // Extract local IP from external address to prevent self-connections
    let local_ip = if let Some(ref mn) = masternode_info {
        Some(mn.address.clone()) // Already IP-only format
    } else {
        // Even non-masternodes should know their public IP to avoid self-connection
        let full_address = config.network.full_external_address(&network_type);
        Some(
            full_address
                .split(':')
                .next()
                .unwrap_or(&full_address)
                .to_string(),
        )
    };

    if let Some(ref ip) = local_ip {
        tracing::info!("🏠 Local public IP detected: {}", ip);
        // Set local IP in peer connection registry for deterministic direction
        peer_connection_registry.set_local_ip(ip.clone());
    }

    // Network client will be started after server is created so we can share resources

    // Register this node if running as masternode
    let masternode_address = masternode_info.as_ref().map(|mn| mn.address.clone());

    if let Some(mn) = masternode_info {
        match registry
            .register(mn.clone(), mn.wallet_address.clone())
            .await
        {
            Ok(()) => {
                // Mark this as our local masternode
                registry.set_local_masternode(mn.address.clone()).await;

                // Set up attestation system identity
                // Generate or load signing key (for now, generate fresh)
                use ed25519_dalek::SigningKey;
                use rand::rngs::OsRng;
                let mut csprng = OsRng;
                let signing_key = SigningKey::from_bytes(&rand::Rng::gen(&mut csprng));
                attestation_system
                    .set_local_identity(mn.address.clone(), signing_key.clone())
                    .await;

                // Set signing key for consensus engine
                if let Err(e) =
                    consensus_engine.set_identity(mn.address.clone(), signing_key.clone())
                {
                    eprintln!("⚠️ Failed to set consensus identity: {}", e);
                }

                tracing::info!("✓ Registered masternode: {}", mn.wallet_address);
                tracing::info!("✓ Heartbeat attestation identity configured");
                tracing::info!("✓ Consensus engine identity configured");

                // Broadcast masternode announcement to the network so peers discover us
                let announcement = NetworkMessage::MasternodeAnnouncement {
                    address: mn.address.clone(),
                    reward_address: mn.wallet_address.clone(),
                    tier: mn.tier,
                    public_key: mn.public_key,
                };
                peer_connection_registry.broadcast(announcement).await;
                tracing::info!("📢 Broadcast masternode announcement to network peers");
            }
            Err(e) => {
                tracing::error!("❌ Failed to register masternode: {}", e);
                std::process::exit(1);
            }
        }

        // Start heartbeat task with attestation
        let registry_clone = registry.clone();
        let attestation_clone = attestation_system.clone();
        let mn_address = mn.address.clone();
        let peer_connection_registry_clone = peer_connection_registry.clone();
        let shutdown_token_clone = shutdown_token.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = shutdown_token_clone.cancelled() => {
                        tracing::debug!("🛑 Heartbeat task shutting down gracefully");
                        break;
                    }
                    _ = interval.tick() => {
                        // Update old-style heartbeat
                        if let Err(e) = registry_clone.heartbeat(&mn_address).await {
                            tracing::warn!("❌ Failed to send heartbeat: {}", e);
                        }

                        // NOTE: Masternode announcements are only sent on initial connection handshake
                        // (see server.rs handle_inbound_peer). Periodic re-announcements cause rate
                        // limit violations (1 per 5min limit) and lead to nodes banning each other.
                        // Discovery happens via GetMasternodes/MasternodesResponse protocol below.

                        // Request masternodes from all connected peers for peer exchange
                        tracing::info!("📤 Broadcasting GetMasternodes to all peers");
                        peer_connection_registry_clone
                            .broadcast(NetworkMessage::GetMasternodes)
                            .await;

                        // Create and broadcast attestable heartbeat
                        match attestation_clone.create_heartbeat().await {
                            Ok(heartbeat) => {
                                tracing::debug!(
                                    "💓 Created signed heartbeat seq {}",
                                    heartbeat.sequence_number
                                );
                                // Broadcast directly through peer connections (not registry channel)
                                let msg = NetworkMessage::HeartbeatBroadcast(heartbeat);
                                peer_connection_registry_clone.broadcast(msg).await;
                            }
                            Err(e) => {
                                tracing::warn!("❌ Failed to create attestable heartbeat: {}", e);
                            }
                        }
                    }
                }
            }
        });
        shutdown_manager.register_task(heartbeat_handle);

        // Start TSDC slot loop for leader election and block production
        let tsdc_loop = tsdc_consensus.clone();
        let consensus_tsdc = consensus_engine.clone();
        let peer_registry_tsdc = peer_connection_registry.clone();
        let masternode_registry_tsdc = registry.clone();
        let blockchain_tsdc = blockchain.clone();
        let shutdown_token_tsdc = shutdown_token.clone();
        let mn_address_tsdc = mn.address.clone();
        let mn_tier = mn.tier;
        let mn_public_key = mn.public_key;

        // Generate VRF keys before spawn (RNG can't cross await)
        use ed25519_dalek::SigningKey;
        use rand::RngCore;
        let mut seed = [0u8; 32];
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut seed);
        let vrf_sk = SigningKey::from_bytes(&seed);
        let vrf_pk = vrf_sk.verifying_key();

        let tsdc_handle = tokio::spawn(async move {
            // Register this node as a TSDC validator
            let validator = tsdc::TSCDValidator {
                id: mn_address_tsdc.clone(),
                public_key: mn_public_key.to_bytes().to_vec(),
                stake: mn_tier.collateral(),
                vrf_secret_key: Some(vrf_sk),
                vrf_public_key: Some(vrf_pk),
            };
            tsdc_loop.set_local_validator(validator).await;

            // Track last proposed slot to prevent duplicate proposals
            let mut last_proposed_slot: Option<u64> = None;

            // Calculate time until next slot boundary
            let slot_duration = 600u64; // 10 minutes
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let current_slot = now / slot_duration;
            let slot_deadline = (current_slot + 1) * slot_duration;
            let sleep_duration = slot_deadline.saturating_sub(now);

            // Wait until next slot boundary
            tokio::time::sleep(tokio::time::Duration::from_secs(sleep_duration)).await;

            let mut slot_interval =
                tokio::time::interval(tokio::time::Duration::from_secs(slot_duration));
            slot_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = shutdown_token_tsdc.cancelled() => {
                        tracing::debug!("🛑 TSDC slot loop shutting down gracefully");
                        break;
                    }
                    _ = slot_interval.tick() => {
                        let current_slot = tsdc_loop.current_slot();

                        // Skip if we already proposed for this slot
                        if last_proposed_slot == Some(current_slot) {
                            tracing::trace!("Already proposed for slot {}, skipping", current_slot);
                            continue;
                        }

                        // Don't produce regular blocks if genesis doesn't exist
                        let genesis_exists = blockchain_tsdc.get_height().await > 0;

                        if !genesis_exists {
                            tracing::trace!("Waiting for genesis block before producing regular blocks");
                            continue;
                        }

                        // Try to become leader for this slot
                        match tsdc_loop.select_leader(current_slot).await {
                            Ok(leader) => {
                                if leader.id == mn_address_tsdc {
                                    // Check if we have enough synced nodes before proposing
                                    // Count connected peers (we're including ourselves implicitly)
                                    let connected_count = peer_registry_tsdc.connected_count();

                                    // Require at least 3 nodes total (including ourselves) for consensus
                                    let required_sync = 3;
                                    let total_nodes = connected_count + 1; // +1 for ourselves

                                    if total_nodes < required_sync {
                                        tracing::warn!(
                                            "⚠️  Not enough synced peers for block proposal: {}/{} required",
                                            total_nodes,
                                            required_sync
                                        );
                                        continue;
                                    }

                                    tracing::info!("🎯 SELECTED AS LEADER for slot {}", current_slot);

                                    // Get finalized transactions from consensus engine
                                    let finalized_txs = consensus_tsdc.get_finalized_transactions_for_block();

                                    // Calculate masternode rewards for all active masternodes
                                    let active_masternodes = masternode_registry_tsdc.get_active_masternodes().await;
                                    const BLOCK_REWARD_SATOSHIS: u64 = 100 * 100_000_000; // 100 TIME
                                    let masternode_rewards: Vec<(String, u64)> = if !active_masternodes.is_empty() {
                                        let per_masternode = BLOCK_REWARD_SATOSHIS / active_masternodes.len() as u64;
                                        active_masternodes.iter()
                                            .map(|mn| (mn.masternode.address.clone(), per_masternode))
                                            .collect()
                                    } else {
                                        vec![]
                                    };

                                    tracing::info!(
                                        "💰 Distributing {} TIME to {} masternodes ({} TIME each)",
                                        BLOCK_REWARD_SATOSHIS as f64 / 100_000_000.0,
                                        active_masternodes.len(),
                                        if !active_masternodes.is_empty() {
                                            (BLOCK_REWARD_SATOSHIS / active_masternodes.len() as u64) as f64 / 100_000_000.0
                                        } else {
                                            0.0
                                        }
                                    );

                                    // Propose block with finalized transactions
                                    match tsdc_loop.propose_block(
                                        mn_address_tsdc.clone(),
                                        finalized_txs.clone(),
                                        masternode_rewards,
                                    ).await {
                                        Ok(block) => {
                                            tracing::info!(
                                                "📦 Proposed block at height {} with {} transactions",
                                                block.header.height,
                                                block.transactions.len()
                                            );

                                            // Mark this slot as proposed
                                            last_proposed_slot = Some(current_slot);

                                            // Broadcast block proposal to all peers
                                            let proposal = NetworkMessage::TSCDBlockProposal { block };
                                            peer_registry_tsdc.broadcast(proposal).await;
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to propose block: {}", e);
                                        }
                                    }
                                } else {
                                    tracing::debug!("Slot {} leader: {}", current_slot, leader.id);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to select leader for slot {}: {}", current_slot, e);
                            }
                        }
                    }
                }
            }
        });
        shutdown_manager.register_task(tsdc_handle);
    }

    // Initialize blockchain and sync from peers in background
    let blockchain_init = blockchain.clone();
    let blockchain_server = blockchain_init.clone();
    let peer_registry_for_sync = peer_connection_registry.clone();
    tokio::spawn(async move {
        // STEP 1: Load genesis from file FIRST (before waiting for peers)
        // Genesis file is local - no network needed
        tracing::info!("📥 Initializing genesis block...");
        if let Err(e) = blockchain_init.initialize_genesis().await {
            tracing::error!(
                "❌ Genesis initialization failed: {} - check that genesis.testnet.json exists",
                e
            );
        }

        // Verify we now have genesis
        let has_genesis = blockchain_init.get_height().await > 0
            || blockchain_init.get_block_by_height(0).await.is_ok();

        if !has_genesis {
            tracing::error!("❌ Failed to load genesis block - cannot proceed");
            return;
        }

        tracing::info!("✓ Genesis block loaded, now syncing remaining blocks from peers");

        // STEP 2: Wait for peer connections to sync remaining blocks
        let mut wait_seconds = 0u64;
        let max_wait = 60u64; // Wait up to 60 seconds for peers
        while wait_seconds < max_wait {
            let connected = peer_registry_for_sync.get_connected_peers().await.len();
            if connected > 0 {
                tracing::info!(
                    "✓ {} peer(s) connected, starting blockchain sync",
                    connected
                );
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
            wait_seconds += 2;
            if wait_seconds % 10 == 0 {
                tracing::info!("⏳ Waiting for peer connections... ({}s)", wait_seconds);
            }
        }

        // STEP 3: Sync remaining blocks from peers
        tracing::info!("📦 Syncing blockchain from peers...");
        if let Err(e) = blockchain_init.sync_from_peers().await {
            tracing::warn!("⚠️  Initial sync from peers: {}", e);
        }

        // Verify chain integrity and download any missing blocks
        if let Err(e) = blockchain_init.ensure_chain_complete().await {
            tracing::warn!("⚠️  Chain integrity check: {}", e);
        }

        // Continue syncing if still behind
        if let Err(e) = blockchain_init.sync_from_peers().await {
            tracing::warn!("⚠️  Block sync from peers: {}", e);
        }

        // Start periodic chain comparison for fork detection
        Blockchain::start_chain_comparison_task(blockchain_init.clone());
        tracing::info!("✓ Fork detection task started (checks every 5 minutes)");

        // Start periodic genesis validation check (in case of late genesis file deployment)
        let blockchain_for_genesis = blockchain_init.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

                // Only check if we don't have a valid genesis yet
                let height = blockchain_for_genesis.get_height().await;
                if height == 0 {
                    // Check if we have a genesis block
                    if blockchain_for_genesis.get_block_by_height(0).await.is_err() {
                        // No genesis - try to load from file
                        if let Err(e) = blockchain_for_genesis.initialize_genesis().await {
                            tracing::debug!("Genesis not ready yet: {}", e);
                        }
                    }
                }
            }
        });

        // Block production is handled by the timer task below
    });

    // Perform initial time check BEFORE starting anything else
    println!("🕐 Checking system time synchronization...");
    let mut time_sync = TimeSync::new();

    match time_sync.check_time_sync().await {
        Ok(offset_ms) => {
            let offset_seconds = offset_ms / 1000;
            if offset_seconds.abs() > 120 {
                eprintln!(
                    "❌ CRITICAL: System time is off by {} seconds",
                    offset_seconds
                );
                eprintln!("   System time must be within 2 minutes of NTP time.");
                eprintln!("   Please synchronize your system clock and try again.");
                std::process::exit(1);
            } else if offset_seconds.abs() > 60 {
                println!(
                    "⚠ WARNING: System time is off by {} seconds",
                    offset_seconds
                );
                println!("  Time will be calibrated, but consider syncing system clock.");
            } else {
                println!("✓ System time is synchronized (offset: {} ms)", offset_ms);
            }
        }
        Err(e) => {
            eprintln!("❌ CRITICAL: Failed to contact NTP server: {}", e);
            eprintln!("   Node requires NTP synchronization to operate correctly.");
            eprintln!("   Please check your network connection and NTP server availability.");
            std::process::exit(1);
        }
    }
    println!();

    // Start background NTP time synchronization
    time_sync.start_sync_task();

    // Peer discovery
    if config.network.enable_peer_discovery {
        let discovery_url = network_type.peer_discovery_url();
        println!("🔍 Discovering peers from {}...", discovery_url);
        let discovery =
            network::peer_discovery::PeerDiscovery::new(discovery_url.to_string(), network_type);

        let fallback_peers = config.network.bootstrap_peers.clone();
        let discovered_peers = discovery.fetch_peers_with_fallback(fallback_peers).await;

        println!("  ✅ Loaded {} peer(s)", discovered_peers.len());
        for peer in discovered_peers.iter().take(3) {
            // Display IP with port (port comes from network type default)
            println!("     • {}:{}", peer.address, peer.port);
        }
        if discovered_peers.len() > 3 {
            println!("     ... and {} more", discovered_peers.len() - 3);
        }
        println!();
    }

    // Start block production timer (every 10 minutes)
    let block_registry = registry.clone();
    let block_blockchain = blockchain.clone();
    let block_peer_registry = peer_connection_registry.clone(); // Used for peer sync before fallback
    let shutdown_token_clone = shutdown_token.clone();
    let tsdc_for_catchup = tsdc_consensus.clone();
    let local_ip_for_catchup = local_ip.clone();

    // Guard flag to prevent duplicate block production (P2P best practice #8)
    let is_producing_block = Arc::new(AtomicBool::new(false));
    let is_producing_block_clone = is_producing_block.clone();

    let block_production_handle = tokio::spawn(async move {
        let is_producing = is_producing_block_clone;

        // Give a moment for initial peer connections and masternode discovery
        // before checking if we're behind (prevents false immediate catchup trigger)
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Check if we're significantly behind - if so, start catchup immediately
        let current_height = block_blockchain.get_height().await;
        let expected_height = block_blockchain.calculate_expected_height();
        let blocks_behind = expected_height.saturating_sub(current_height);

        let initial_wait = if blocks_behind > 10 {
            // More than 10 blocks behind - start catchup immediately
            tracing::info!(
                "⚡ {} blocks behind - starting immediate catchup (bypassing 10-min boundary wait)",
                blocks_behind
            );
            0
        } else {
            // Calculate time until next 10-minute boundary for normal operation
            let now = chrono::Utc::now();
            let minute = now.minute();
            let seconds_into_period = (minute % 10) * 60 + now.second();
            600 - seconds_into_period
        };

        // Wait until the next 10-minute boundary (or start immediately if behind)
        if initial_wait > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(initial_wait as u64)).await;
        }

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(600)); // 10 minutes
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown_token_clone.cancelled() => {
                    tracing::debug!("🛑 Block production task shutting down gracefully");
                    break;
                }
                _ = interval.tick() => {
                    // Mark start of new block period
                    block_registry.start_new_block_period().await;

                    let now = chrono::Utc::now();
                    let timestamp = now
                        .date_naive()
                        .and_hms_opt(now.hour(), (now.minute() / 10) * 10, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp();

                    // Get masternodes eligible for rewards (active for entire block period)
                    let eligible = block_registry.get_eligible_for_rewards().await;
                    let mut masternodes: Vec<Masternode> = eligible.iter().map(|(mn, _)| mn.clone()).collect();
                    // Sort deterministically by address for consistent leader election across all nodes
                    sort_masternodes_canonical(&mut masternodes);

                    let current_height = block_blockchain.get_height().await;
                    let expected_height = block_blockchain.calculate_expected_height();

                    // Allow single-node bootstrap during initial catchup (height 0)
                    // After genesis, require at least 3 masternodes for normal operation
                    if masternodes.len() < 3 && current_height > 0 {
                        tracing::warn!(
                            "⚠️ Skipping block production: only {} masternodes active (minimum 3 required for post-genesis blocks)",
                            masternodes.len()
                        );
                        continue;
                    }

                    // During initial bootstrap (height 0), allow 1 masternode to produce blocks
                    if masternodes.is_empty() {
                        tracing::warn!("⚠️ Skipping block production: no masternodes registered");
                        continue;
                    }

                    // Determine what to do based on height comparison
                    if current_height < expected_height - 1 {
                        // More than 1 block behind - need catchup
                        let blocks_behind = expected_height - current_height;
                        tracing::info!(
                            "🧱 Catching up: height {} → {} ({} blocks behind) at {} ({}:{}0) with {} eligible masternodes",
                            current_height,
                            expected_height,
                            blocks_behind,
                            timestamp,
                            now.hour(),
                            (now.minute() / 10),
                            masternodes.len()
                        );

                        // First, try to sync from peers
                        match block_blockchain.sync_from_peers().await {
                            Ok(()) => {
                                tracing::info!("✅ Sync complete");
                                continue; // Synced successfully, check again next tick
                            }
                            Err(e) => {
                                tracing::warn!("⚠️  Sync from peers failed: {}", e);
                                // Continue to catchup block production - peers may also be behind
                            }
                        }

                        // Sync failed - all peers may also be behind
                        // Use TSDC leader selection for catchup blocks (use current_height as slot)
                        let catchup_slot = current_height;
                        let (is_leader, leader_address) = match tsdc_for_catchup.select_leader(catchup_slot).await {
                            Ok(leader) => {
                                tracing::info!("🗳️  Catchup leader selected: {} for slot {}", leader.id, catchup_slot);
                                let leader_addr = leader.id.clone();
                                let my_ip = local_ip_for_catchup.as_deref().unwrap_or("");
                                let is_leader = my_ip == leader_addr;
                                (is_leader, leader_addr)
                            }
                            Err(e) => {
                                tracing::warn!("⚠️  Failed to select catchup leader: {}", e);
                                (false, String::from("unknown"))
                            }
                        };

                        if is_leader {
                            // Acquire block production lock (P2P best practice #8)
                            if is_producing.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                                tracing::warn!("⚠️  Block production already in progress, skipping catchup");
                                continue;
                            }

                            tracing::info!(
                                "🎯 Elected as catchup leader - producing {} blocks to reach height {}",
                                blocks_behind,
                                expected_height
                            );

                            // Produce catchup blocks with rate limiting
                            let mut catchup_produced = 0u64;
                            let genesis_timestamp = block_blockchain.genesis_timestamp();

                            for target_height in (current_height + 1)..=expected_height {
                                // CRITICAL: Enforce time-based schedule even in catch-up mode
                                let expected_timestamp =
                                    genesis_timestamp + (target_height as i64 * 600);
                                let now = chrono::Utc::now().timestamp();

                                if expected_timestamp > now {
                                    // We've caught up to real time - stop producing
                                    tracing::info!(
                                        "⏰ Reached real-time at height {} (expected time: {}, now: {})",
                                        target_height - 1,
                                        expected_timestamp,
                                        now
                                    );
                                    break;
                                }

                                match block_blockchain.produce_block().await {
                                    Ok(block) => {
                                        // Add block to our chain
                                        if let Err(e) = block_blockchain.add_block(block.clone()).await {
                                            tracing::error!("❌ Catchup block {} failed: {}", target_height, e);
                                            break;
                                        }

                                        // Broadcast to peers
                                        block_registry.broadcast_block(block.clone()).await;
                                        catchup_produced += 1;

                                        if catchup_produced % 10 == 0 || block.header.height == expected_height {
                                            tracing::info!(
                                                "📦 Catchup progress: {}/{} blocks (height: {})",
                                                catchup_produced,
                                                blocks_behind,
                                                block.header.height
                                            );
                                        }

                                        // Network propagation delay - not too fast, not too slow
                                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                                    }
                                    Err(e) => {
                                        // Check if error is due to timestamp in future
                                        if e.contains("timestamp") && e.contains("future") {
                                            tracing::info!("⏰ Catch-up stopped: reached real-time schedule");
                                            break;
                                        }
                                        tracing::error!("❌ Failed to produce catchup block: {}", e);
                                        break;
                                    }
                                }
                            }

                            // Release block production lock
                            is_producing.store(false, Ordering::SeqCst);

                            tracing::info!(
                                "✅ Catchup complete: produced {} blocks, height now: {}",
                                catchup_produced,
                                block_blockchain.get_height().await
                            );
                        } else {
                            tracing::info!(
                                "⏳ Waiting for catchup leader {} to produce blocks (30s timeout)",
                                leader_address
                            );

                            // Wait for leader to produce blocks
                            let height_before = block_blockchain.get_height().await;
                            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                            let height_after = block_blockchain.get_height().await;

                            // If height didn't change, leader failed - try syncing from peers first
                            if height_after == height_before {
                                let _our_height = block_blockchain.get_height().await;
                                let expected_height = block_blockchain.calculate_expected_height();

                                // Before producing blocks locally, try harder to sync from connected peers
                                let connected_peers = block_peer_registry.list_peers().await;
                                if !connected_peers.is_empty() {
                                    tracing::info!(
                                        "🔄 Requesting blocks from {} connected peer(s) before fallback production",
                                        connected_peers.len()
                                    );

                                    // Try syncing multiple times before giving up
                                    let mut sync_attempts = 0;
                                    let max_sync_attempts = 5; // Try 5 times before fallback

                                    while sync_attempts < max_sync_attempts {
                                        let current_height = block_blockchain.get_height().await;

                                        // Request blocks - always start from 0 when at height 0
                                        let start_height = if current_height == 0 {
                                            0  // Always request genesis when at height 0
                                        } else {
                                            current_height + 1  // Normal case
                                        };

                                        // Request blocks from all connected peers
                                        let get_blocks = NetworkMessage::GetBlocks(start_height, expected_height);
                                        for peer_ip in &connected_peers {
                                            let _ = block_peer_registry.send_to_peer(peer_ip, get_blocks.clone()).await;
                                        }

                                        // Wait longer for blocks to arrive (30 seconds per attempt)
                                        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

                                        let new_height = block_blockchain.get_height().await;
                                        if new_height > current_height {
                                            let synced_blocks = new_height - current_height;
                                            tracing::info!(
                                                "✅ Synced {} blocks from peers (height: {} → {})",
                                                synced_blocks, current_height, new_height
                                            );

                                            // Check if we're caught up now
                                            if new_height >= expected_height {
                                                tracing::info!("✅ Fully synced via peers, skipping fallback production");
                                                is_producing.store(false, Ordering::SeqCst);
                                                continue; // Fully synced, skip fallback production
                                            }

                                            // Made progress, reset attempt counter
                                            sync_attempts = 0;
                                            tracing::info!("📊 Partial sync successful, still {} blocks behind", expected_height - new_height);
                                        } else {
                                            sync_attempts += 1;
                                            tracing::warn!(
                                                "⚠️  Peer sync attempt {}/{} didn't provide blocks",
                                                sync_attempts, max_sync_attempts
                                            );

                                            if sync_attempts < max_sync_attempts {
                                                tracing::info!("🔄 Retrying peer sync in 10 seconds...");
                                                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                                            }
                                        }
                                    }

                                    // After all attempts, check final status
                                    let final_height = block_blockchain.get_height().await;
                                    if final_height >= expected_height {
                                        tracing::info!("✅ Caught up via peer sync, skipping fallback");
                                        is_producing.store(false, Ordering::SeqCst);
                                        continue;
                                    }

                                    let blocks_still_behind = expected_height - final_height;
                                    tracing::warn!(
                                        "⚠️  After {} sync attempts, still {} blocks behind",
                                        max_sync_attempts, blocks_still_behind
                                    );

                                    // Allow fallback production if:
                                    // 1. Catchup leader has timed out (we waited 30s)
                                    // 2. Peer sync exhausted (tried 5 times)
                                    // 3. Still far behind (>10 blocks)
                                    // This prevents permanent deadlock while maintaining safety
                                    if blocks_still_behind > 10 {
                                        tracing::info!("🔧 Catchup leader timed out and peers don't have blocks - will attempt local block production to recover");
                                        // Continue to fallback production below
                                    } else {
                                        tracing::warn!(
                                            "⚠️  Waiting for peers to provide remaining {} blocks",
                                            blocks_still_behind
                                        );
                                        is_producing.store(false, Ordering::SeqCst);
                                        continue; // Only skip fallback if close to catching up
                                    }
                                } else {
                                    tracing::warn!("⚠️  No connected peers available for sync");
                                    // Continue to fallback production below
                                }

                                // FALLBACK PRODUCTION: If catchup leader times out and peer sync fails,
                                // attempt local block production to recover the network

                                // Acquire block production lock (P2P best practice #8)
                                if is_producing.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                                    tracing::warn!("⚠️  Block production already in progress, skipping fallback catchup");
                                    continue;
                                }

                                tracing::warn!(
                                    "⚠️  Catchup leader timeout - becoming fallback producer"
                                );

                                // Produce catchup blocks ourselves (only after sync attempt failed)
                                let mut catchup_produced = 0u64;
                                let current_height = block_blockchain.get_height().await;
                                let genesis_timestamp = block_blockchain.genesis_timestamp();

                                for target_height in (current_height + 1)..=expected_height {
                                    // CRITICAL: Enforce time-based schedule even in fallback catch-up
                                    let expected_timestamp =
                                        genesis_timestamp + (target_height as i64 * 600);
                                    let now = chrono::Utc::now().timestamp();

                                    if expected_timestamp > now {
                                        // We've caught up to real time - stop producing
                                        tracing::info!(
                                            "⏰ Fallback catch-up reached real-time at height {}",
                                            target_height - 1
                                        );
                                        break;
                                    }

                                    match block_blockchain.produce_block().await {
                                        Ok(block) => {
                                            if let Err(e) = block_blockchain.add_block(block.clone()).await {
                                                tracing::error!("❌ Fallback catchup block {} failed: {}", block.header.height, e);
                                                break;
                                            }

                                            block_registry.broadcast_block(block.clone()).await;
                                            catchup_produced += 1;

                                            if catchup_produced % 10 == 0 {
                                                tracing::info!(
                                                    "📦 Fallback catchup progress: {} blocks produced",
                                                    catchup_produced
                                                );
                                            }

                                            // Network propagation delay
                                            tokio::time::sleep(tokio::time::Duration::from_millis(500))
                                                .await;
                                        }
                                        Err(e) => {
                                            // Check if error is due to timestamp in future
                                            if e.contains("timestamp") && e.contains("future") {
                                                tracing::info!(
                                                    "⏰ Fallback catch-up stopped: reached real-time schedule"
                                                );
                                                break;
                                            }
                                            tracing::error!("❌ Failed to produce fallback catchup block: {}", e);
                                            break;
                                        }
                                    }
                                }

                                // Release block production lock
                                is_producing.store(false, Ordering::SeqCst);

                                tracing::info!(
                                    "✅ Fallback catchup complete: produced {} blocks, height now: {}",
                                    catchup_produced,
                                    block_blockchain.get_height().await
                                );

                            } else {
                                tracing::info!(
                                    "✅ Leader produced blocks, height: {} → {}",
                                    height_before,
                                    height_after
                                );
                            }
                        }
                    } else if current_height == expected_height - 1 || current_height == expected_height {
                        // At expected height or one behind (normal) - determine if we should produce

                        // Use VDF to select block producer deterministically
                        let prev_block_hash = match block_blockchain.get_block_hash(current_height) {
                            Ok(hash) => hash,
                            Err(e) => {
                                tracing::error!("Failed to get previous block hash: {}", e);
                                continue;
                            }
                        };

                        // Use deterministic leader selection based on previous block hash
                        // This provides fair rotation without expensive VDF computation
                        use sha2::{Digest, Sha256};
                        let mut hasher = Sha256::new();
                        hasher.update(prev_block_hash);
                        hasher.update(current_height.to_le_bytes());
                        let selection_hash: [u8; 32] = hasher.finalize().into();

                        // Select producer: hash mod masternode_count
                        let producer_index = {
                            let mut val = 0u64;
                            for (i, &byte) in selection_hash.iter().take(8).enumerate() {
                                val |= (byte as u64) << (i * 8);
                            }
                            (val % masternodes.len() as u64) as usize
                        };

                        let selected_producer = &masternodes[producer_index];
                        let is_producer = masternode_address
                            .as_ref()
                            .map(|addr| addr == &selected_producer.address)
                            .unwrap_or(false);

                        if is_producer {
                            // Validate chain time before producing
                            if let Err(e) = block_blockchain.validate_chain_time().await {
                                tracing::warn!("⚠️  Chain time validation failed: {}", e);
                                tracing::warn!("⚠️  Skipping block production until time catches up");
                                continue;
                            }

                            // Check if we have enough synced peers (minimum 2 peers = 3 nodes total including us)
                            let connected_peers = block_peer_registry.get_connected_peers().await;
                            if connected_peers.len() < 2 {
                                tracing::warn!(
                                    "⚠️ Skipping block production: only {} connected peer(s) (minimum 2 required for 3-node sync)",
                                    connected_peers.len()
                                );
                                continue;
                            }

                            // Acquire block production lock (P2P best practice #8)
                            if is_producing.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
                                tracing::warn!("⚠️  Block production already in progress, skipping");
                                continue;
                            }

                            tracing::info!(
                                "🎯 Selected as block producer for height {} at {} ({}:{}0)",
                                current_height + 1,
                                timestamp,
                                now.hour(),
                                (now.minute() / 10),
                            );

                            match block_blockchain.produce_block().await {
                                Ok(block) => {
                                    let block_height = block.header.height;
                                    tracing::info!(
                                        "✅ Block {} produced: {} transactions, {} masternode rewards",
                                        block_height,
                                        block.transactions.len(),
                                        block.masternode_rewards.len()
                                    );

                                    // Add block to our own chain first
                                    if let Err(e) = block_blockchain.add_block(block.clone()).await {
                                        tracing::error!("❌ Failed to add block to chain: {}", e);
                                        is_producing.store(false, Ordering::SeqCst);
                                        continue;
                                    }

                                    tracing::info!(
                                        "✅ Block {} added to chain, height now: {}",
                                        block_height,
                                        block_blockchain.get_height().await
                                    );

                                    // Broadcast block to all peers
                                    block_registry.broadcast_block(block).await;
                                    tracing::info!("📡 Block {} broadcast to peers", block_height);
                                }
                                Err(e) => {
                                    tracing::error!("❌ Failed to produce block: {}", e);
                                }
                            }

                            // Release block production lock
                            is_producing.store(false, Ordering::SeqCst);
                        } else {
                            tracing::debug!(
                                "⏸️  Not selected for block {} (producer: {})",
                                current_height + 1,
                                selected_producer.address
                            );
                        }
                    } else {
                        // Height is ahead of expected - this can happen if:
                        // 1. Clock skew caused blocks to be produced early
                        // 2. We received blocks from a peer with clock skew
                        // Just wait silently for time to catch up - only log once per minute
                        static LAST_AHEAD_LOG: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
                        let now_secs = chrono::Utc::now().timestamp();
                        let last_log = LAST_AHEAD_LOG.load(Ordering::Relaxed);
                        if now_secs - last_log >= 60 {
                            LAST_AHEAD_LOG.store(now_secs, Ordering::Relaxed);
                            let blocks_ahead = current_height.saturating_sub(expected_height);
                            let wait_minutes = blocks_ahead * 10; // 10 minutes per block
                            tracing::info!(
                                "⏳ Chain height {} is {} blocks ahead of time, waiting ~{} minutes for time to catch up",
                                current_height,
                                blocks_ahead,
                                wait_minutes
                            );
                        }
                    }
                }
            }
        }
    });
    shutdown_manager.register_task(block_production_handle);

    // Start network server

    println!("🌐 Starting P2P network server...");

    // Start RPC server
    let rpc_consensus = consensus_engine.clone();
    let rpc_utxo = utxo_mgr.clone();
    let rpc_registry = registry.clone();
    let rpc_blockchain = blockchain.clone();
    let rpc_addr_clone = rpc_addr.clone();
    let rpc_network = network_type;
    let rpc_shutdown_token = shutdown_token.clone();
    let rpc_attestation = attestation_system.clone();

    let rpc_handle = tokio::spawn(async move {
        match RpcServer::new(
            &rpc_addr_clone,
            rpc_consensus,
            rpc_utxo,
            rpc_network,
            rpc_registry,
            rpc_blockchain,
            rpc_attestation,
        )
        .await
        {
            Ok(mut server) => {
                tokio::select! {
                    _ = rpc_shutdown_token.cancelled() => {
                        tracing::debug!("🛑 RPC server shutting down gracefully");
                    }
                    result = server.run() => {
                        if let Err(e) = result {
                            eprintln!("RPC server error: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("  ❌ Failed to start RPC server: {}", e);
            }
        }
    });
    shutdown_manager.register_task(rpc_handle);

    // Periodic status report - logs every 5 minutes at :00, :05, :10, :15, :20, :25, :30, :35, :40, :45, :50, :55
    let status_blockchain = blockchain_server.clone();
    let status_registry = registry.clone();
    let shutdown_token_clone = shutdown_token.clone();
    let status_handle = tokio::spawn(async move {
        loop {
            // Wait until next 5-minute mark (:00, :05, :10, :15, :20, :25, :30, :35, :40, :45, :50, :55)
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let minute = (now / 60) % 60;
            let second = now % 60;

            // Calculate seconds until next 5-minute mark
            let next_5min_mark = ((minute / 5) + 1) * 5;
            let target_minute = if next_5min_mark >= 60 {
                0
            } else {
                next_5min_mark
            };

            let minutes_until = if target_minute > minute {
                target_minute - minute
            } else {
                60 - minute + target_minute
            };

            let seconds_until = (minutes_until * 60) - second;

            tokio::select! {
                _ = shutdown_token_clone.cancelled() => {
                    tracing::debug!("🛑 Status report task shutting down gracefully");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(seconds_until)) => {
                    let height = status_blockchain.get_height().await;
                    let mn_count = status_registry.list_active().await.len();
                    tracing::info!(
                        "📊 Status: Height={}, Active Masternodes={}",
                        height,
                        mn_count
                    );
                }
            }
        }
    });
    shutdown_manager.register_task(status_handle);

    match NetworkServer::new_with_blacklist(
        &p2p_addr,
        utxo_mgr.clone(),
        consensus_engine.clone(),
        registry.clone(),
        blockchain_server.clone(),
        peer_manager.clone(),
        connection_manager.clone(),
        peer_connection_registry.clone(),
        peer_state.clone(),
        local_ip.clone(),
        config.network.blacklisted_peers.clone(),
        attestation_system.clone(),
    )
    .await
    {
        Ok(mut server) => {
            // Give registry access to network broadcast channel
            registry
                .set_broadcast_channel(server.tx_notifier.clone())
                .await;

            // Share TSDC resources with peer connection registry for outbound connections
            peer_connection_registry
                .set_tsdc_resources(
                    consensus_engine.clone(),
                    server.block_cache.clone(),
                    server.tx_notifier.clone(),
                )
                .await;

            println!("  ✅ Network server listening on {}", p2p_addr);

            // Now create network client for outbound connections
            let network_client = network::client::NetworkClient::new(
                peer_manager.clone(),
                registry.clone(),
                blockchain.clone(),
                attestation_system.clone(),
                network_type,
                config.network.max_peers as usize,
                peer_connection_registry.clone(),
                peer_state.clone(),
                connection_manager.clone(),
                local_ip.clone(),
                config.network.blacklisted_peers.clone(),
            );
            network_client.start().await;
            println!("\n╔═══════════════════════════════════════════════════════╗");
            println!("║  🎉 TIME Coin Daemon is Running!                      ║");
            println!("╠═══════════════════════════════════════════════════════╣");
            println!("║  Network:    {:<40} ║", format!("{:?}", network_type));
            println!("║  Storage:    {:<40} ║", config.storage.backend);
            println!("║  P2P Port:   {:<40} ║", p2p_addr);
            println!("║  RPC Port:   {:<40} ║", rpc_addr);
            println!("║  Consensus:  TSDC + Avalanche Hybrid                  ║");
            println!("║  Finality:   Instant (<10 seconds)                    ║");
            println!("╚═══════════════════════════════════════════════════════╝");
            println!("\nPress Ctrl+C to stop\n");

            let shutdown_token_net = shutdown_token.clone();
            let server_handle = tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown_token_net.cancelled() => {
                        tracing::debug!("🛑 Network server shutting down gracefully");
                    }
                    result = server.run() => {
                        if let Err(e) = result {
                            println!("❌ Server error: {}", e);
                        }
                    }
                }
            });
            shutdown_manager.register_task(server_handle);

            // Wait for shutdown signal
            shutdown_manager.wait_for_shutdown().await;
        }
        Err(e) => {
            println!("  ❌ Failed to start network: {}", e);
            println!("     (Port may already be in use)");
            println!("\n✓ Core components initialized successfully!");
        }
    }
}

fn setup_logging(config: &config::LoggingConfig, verbose: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    let level = if verbose { "trace" } else { &config.level };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    // Detect if running under systemd/journald
    let is_systemd =
        std::env::var("JOURNAL_STREAM").is_ok() || std::env::var("INVOCATION_ID").is_ok();

    // Get hostname - shorten to first part before dot
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    let short_hostname = hostname.split('.').next().unwrap_or(&hostname).to_string();

    match config.format.as_str() {
        "json" => {
            fmt()
                .json()
                .with_env_filter(filter)
                .with_thread_ids(false)
                .init();
        }
        _ => {
            if is_systemd {
                // When running under systemd, don't include timestamp/hostname
                // (journald already adds them)
                fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .with_thread_ids(false)
                    .with_thread_names(false)
                    .with_file(false)
                    .with_line_number(false)
                    .without_time()
                    .compact()
                    .init();
            } else {
                // When running manually, include custom timer with hostname
                fmt()
                    .with_env_filter(filter)
                    .with_target(false)
                    .with_thread_ids(false)
                    .with_thread_names(false)
                    .with_file(false)
                    .with_line_number(false)
                    .with_timer(CustomTimer {
                        hostname: short_hostname,
                    })
                    .compact()
                    .init();
            }
        }
    }
}

// Custom timer that shows UTC time and hostname
struct CustomTimer {
    hostname: String,
}

impl tracing_subscriber::fmt::time::FormatTime for CustomTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        // Get current UTC time using chrono (system time)
        use chrono::Utc;
        let now = Utc::now();

        // Format: "YYYY-MM-DD HH:MM:SS.mmm [hostname]"
        // Example: "2025-12-10 18:09:43.150 [server1]"
        write!(
            w,
            "{}.{:03} [{}]",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.timestamp_subsec_millis(),
            self.hostname
        )
    }
}
