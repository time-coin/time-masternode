pub mod address;
pub mod ai;
pub mod avalanche;
pub mod block;
pub mod blockchain;
pub mod config;
pub mod consensus;
pub mod crypto;
pub mod error;
pub mod finality_proof;
pub mod heartbeat_attestation;
pub mod masternode_registry;
pub mod network;
pub mod network_type;
pub mod peer_manager;
pub mod rpc;
pub mod shutdown;
pub mod state_notifier;
pub mod storage;
pub mod time_sync;
pub mod transaction_pool;
pub mod tsdc;
pub mod types;
pub mod utxo_manager;
pub mod wallet;

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
    let peer_db = match sled::Config::new()
        .path(format!("{}/peers", db_dir))
        .cache_capacity(cache_size)
        .open()
    {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!("❌ Failed to open peer database: {}", e);
            eprintln!("   Check disk space and file permissions");
            std::process::exit(1);
        }
    };
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
    let mut consensus_engine = ConsensusEngine::new(vec![], utxo_mgr.clone());

    // Enable AI validation using the same db as block storage
    consensus_engine.enable_ai_validation(Arc::new(block_storage.clone()));

    let consensus_engine = Arc::new(consensus_engine);
    tracing::info!("✓ Consensus engine initialized with AI validation");

    // Initialize AI Masternode Health Monitor (if enabled)
    let health_ai = if config.ai.enabled && config.ai.masternode_health.enabled {
        match crate::ai::MasternodeHealthAI::new(
            Arc::new(block_storage.clone()),
            config.ai.learning_rate,
            config.ai.min_samples,
        ) {
            Ok(ai) => {
                tracing::info!("✓ AI Masternode Health Monitor initialized");
                Some(Arc::new(ai))
            }
            Err(e) => {
                tracing::warn!("⚠️  Failed to initialize AI health monitor: {}", e);
                None
            }
        }
    } else {
        tracing::info!("AI Masternode Health Monitor disabled in config");
        None
    };

    // Initialize TSDC consensus engine with masternode registry
    let mut tsdc_consensus =
        TSCDConsensus::with_masternode_registry(Default::default(), registry.clone());

    // Set AI health monitor if available
    if let Some(ref ai) = health_ai {
        tsdc_consensus.set_health_ai(ai.clone());
        tracing::info!("✓ TSDC consensus engine initialized with AI health monitoring");
    } else {
        tracing::info!("✓ TSDC consensus engine initialized (no AI)");
    }

    let tsdc_consensus = Arc::new(tsdc_consensus);

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

    // Validate chain integrity on startup and auto-heal if needed
    match blockchain.validate_chain_integrity().await {
        Ok(corrupt_blocks) => {
            if !corrupt_blocks.is_empty() {
                tracing::error!(
                    "❌ Chain integrity check failed: {} corrupt blocks detected",
                    corrupt_blocks.len()
                );
                // Delete corrupt blocks to trigger re-sync
                if let Err(e) = blockchain.delete_corrupt_blocks(&corrupt_blocks).await {
                    tracing::error!("❌ Failed to delete corrupt blocks: {}", e);
                } else {
                    tracing::info!("✅ Corrupt blocks deleted - will re-sync from peers");
                }
            } else {
                tracing::info!("✅ Chain integrity validation passed");
            }
        }
        Err(e) => {
            tracing::error!("❌ Chain integrity validation error: {}", e);
        }
    }

    // Cleanup blocks with invalid merkle roots (00000...)
    // This removes blocks produced before the mempool population fix
    match blockchain.cleanup_invalid_merkle_blocks().await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("✅ Removed {} block(s) with invalid merkle roots", count);
            }
        }
        Err(e) => {
            tracing::error!("❌ Failed to cleanup invalid merkle blocks: {}", e);
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

    // Set connection manager on blockchain for reward distribution
    blockchain
        .set_connection_manager(connection_manager.clone())
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
        let blockchain_clone = blockchain.clone();
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
                        let block_height = blockchain_clone.get_height().await;
                        match attestation_clone.create_heartbeat(block_height).await {
                            Ok(heartbeat) => {
                                tracing::debug!(
                                    "💓 Created signed heartbeat seq {} at height {}",
                                    heartbeat.sequence_number,
                                    heartbeat.block_height
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

        // STEP 3: Start fork detection BEFORE syncing (run immediately then every 1 min)
        Blockchain::start_chain_comparison_task(blockchain_init.clone());
        tracing::info!("✓ Fork detection task started (checks immediately, then every 1 minute)");

        // Run initial fork detection before syncing
        tracing::info!("🔍 Running initial fork detection...");
        if let Some((consensus_height, consensus_peer)) =
            blockchain_init.compare_chain_with_peers().await
        {
            tracing::info!(
                "🔀 Detected fork: syncing from consensus peer {} at height {}",
                consensus_peer,
                consensus_height
            );
            // Sync specifically from the consensus peer
            if let Err(e) = blockchain_init
                .sync_from_specific_peer(&consensus_peer)
                .await
            {
                tracing::warn!(
                    "⚠️  Failed to sync from consensus peer {}: {}",
                    consensus_peer,
                    e
                );
            }
        }

        // STEP 4: Sync remaining blocks from peers
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

        // Start periodic chain integrity check (every 10 minutes at block time)
        let blockchain_for_integrity = blockchain_init.clone();
        tokio::spawn(async move {
            // Wait for initial sync to complete
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

            loop {
                // Run integrity check every 10 minutes (block time)
                tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;

                tracing::debug!("🔍 Running periodic chain integrity check...");
                match blockchain_for_integrity.validate_chain_integrity().await {
                    Ok(corrupt_blocks) => {
                        if !corrupt_blocks.is_empty() {
                            tracing::error!(
                                "❌ CORRUPTION DETECTED: {} corrupt blocks found: {:?}",
                                corrupt_blocks.len(),
                                corrupt_blocks
                            );
                            // Auto-heal: delete corrupt blocks to trigger re-sync
                            if let Err(e) = blockchain_for_integrity
                                .delete_corrupt_blocks(&corrupt_blocks)
                                .await
                            {
                                tracing::error!("❌ Failed to delete corrupt blocks: {}", e);
                            } else {
                                tracing::info!(
                                    "🔧 Auto-healing: deleted {} corrupt blocks, will re-sync from peers",
                                    corrupt_blocks.len()
                                );
                            }
                        } else {
                            tracing::debug!("✅ Chain integrity check passed");
                        }
                    }
                    Err(e) => {
                        tracing::error!("❌ Chain integrity check error: {}", e);
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

    // Peer discovery - save discovered peers for whitelisting later
    let discovered_peer_ips: Vec<String> = if config.network.enable_peer_discovery {
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

        // Collect IPs for whitelisting (these are from time-coin.io, so trusted)
        discovered_peers.iter().map(|p| p.address.clone()).collect()
    } else {
        Vec::new()
    };

    // Start block production timer (every 10 minutes)
    let block_registry = registry.clone();
    let block_blockchain = blockchain.clone();
    let block_peer_registry = peer_connection_registry.clone(); // Used for peer sync before fallback
    let block_tsdc = tsdc_consensus.clone(); // For TSDC leader selection in catchup
    let block_masternode_address = masternode_address.clone(); // For leader comparison
    let shutdown_token_block = shutdown_token.clone();

    // Guard flag to prevent duplicate block production (P2P best practice #8)
    let is_producing_block = Arc::new(AtomicBool::new(false));
    let is_producing_block_clone = is_producing_block.clone();

    // Trigger for immediate catchup block production (when 5-min status check detects need)
    let catchup_trigger = Arc::new(tokio::sync::Notify::new());
    let catchup_trigger_producer = catchup_trigger.clone();

    let block_production_handle = tokio::spawn(async move {
        let is_producing = is_producing_block_clone;

        // Track catchup leader timeout
        // Maps expected_height -> (leader_id, selection_timestamp, attempt_number)
        let mut catchup_leader_tracker: std::collections::HashMap<
            u64,
            (String, std::time::Instant, u64),
        > = std::collections::HashMap::new();
        // Reduced timeout for faster rotation when network is stalled
        // Leaders have 30 seconds to produce blocks before we rotate to backup
        let leader_timeout = std::time::Duration::from_secs(30);

        // Track last sync time to prevent immediate catchup leader selection
        // After syncing from peers, node needs time to populate mempool via p2p gossip
        // Otherwise produces blocks with empty mempool (00000 merkle roots)
        let mut last_sync_time: Option<std::time::Instant> = None;
        let min_time_after_sync = std::time::Duration::from_secs(60); // 60s to populate mempool

        // Give time for initial blockchain sync to complete before starting block production
        // This prevents race conditions where both init sync and production loop call sync_from_peers()
        tokio::time::sleep(tokio::time::Duration::from_secs(120)).await;

        // Time-based catchup trigger: Check if we're behind schedule
        // Use time rather than block count to determine when to trigger catchup
        let current_height = block_blockchain.get_height().await;
        let expected_height = block_blockchain.calculate_expected_height();
        let blocks_behind = expected_height.saturating_sub(current_height);

        let genesis_timestamp = block_blockchain.genesis_timestamp();
        let now_timestamp = chrono::Utc::now().timestamp();

        // Calculate when the current expected block should have been produced
        let expected_block_time = genesis_timestamp + (expected_height as i64 * 600);
        let time_since_expected = now_timestamp - expected_block_time;

        // Smart catchup trigger:
        // - If many blocks behind (>3): Start immediately regardless of time
        // - If few blocks behind (1-3): Use 5-minute grace period
        let catchup_delay_threshold = 300; // 5 minutes in seconds

        let initial_wait = if blocks_behind > 2 {
            // More than 2 blocks behind - start catchup immediately
            tracing::info!(
                "⚡ {} blocks behind - starting immediate TSDC catchup (>2 blocks threshold)",
                blocks_behind
            );
            0
        } else if blocks_behind > 0 && time_since_expected >= catchup_delay_threshold {
            // 1-2 blocks behind AND 5+ minutes past when block should have been produced
            // Start catchup immediately - normal production had its chance
            tracing::info!(
                "⚡ {} blocks behind, {}s past expected block time - starting immediate TSDC catchup",
                blocks_behind,
                time_since_expected
            );
            0
        } else if blocks_behind > 0 {
            // Exactly 1 block behind and within the 5-minute grace period
            // Wait a bit longer to give normal production a chance
            let remaining_grace = catchup_delay_threshold - time_since_expected;
            tracing::info!(
                "⏳ {} blocks behind but only {}s past expected time - waiting {}s before catchup",
                blocks_behind,
                time_since_expected,
                remaining_grace
            );
            remaining_grace.max(30) as u64 // Wait at least 30s
        } else {
            // Not behind - calculate time until next 10-minute boundary for normal operation
            let now = chrono::Utc::now();
            let minute = now.minute();
            let seconds_into_period = (minute % 10) * 60 + now.second();
            (600 - seconds_into_period) as u64
        };

        // Wait until the appropriate time (or start immediately if past catchup threshold)
        if initial_wait > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(initial_wait as u64)).await;
        }

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(600)); // 10 minutes
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Track whether we were triggered vs regular interval
            // Triggered = status check already tried sync, skip to production
            // Regular = normal flow, attempt sync first
            let triggered_by_status_check: bool;

            tokio::select! {
                _ = shutdown_token_block.cancelled() => {
                    tracing::debug!("🛑 Block production task shutting down gracefully");
                    break;
                }
                _ = catchup_trigger_producer.notified() => {
                    // Triggered by 5-minute status check when catchup is needed
                    tracing::info!("🔔 Catchup production triggered by status check");
                    triggered_by_status_check = true;
                    // Fall through to production logic below
                }
                _ = interval.tick() => {
                    triggered_by_status_check = false;
                    // Mark start of new block period (regular interval only)
                }
            }

            // Production logic continues here for both triggered and regular cases
            if !triggered_by_status_check {
                block_registry.start_new_block_period().await;
            }

            let now = chrono::Utc::now();
            let timestamp = now
                .date_naive()
                .and_hms_opt(now.hour(), (now.minute() / 10) * 10, 0)
                .unwrap()
                .and_utc()
                .timestamp();

            // Get masternodes eligible for rewards (active for entire block period)
            let eligible = block_registry.get_eligible_for_rewards().await;
            let mut masternodes: Vec<Masternode> =
                eligible.iter().map(|(mn, _)| mn.clone()).collect();
            // Sort deterministically by address for consistent leader election across all nodes
            sort_masternodes_canonical(&mut masternodes);

            let current_height = block_blockchain.get_height().await;
            let expected_height = block_blockchain.calculate_expected_height();

            // Determine what to do based on height and time comparison
            let blocks_behind = expected_height.saturating_sub(current_height);

            // Calculate time-based catchup trigger
            let genesis_timestamp = block_blockchain.genesis_timestamp();
            let now_timestamp = chrono::Utc::now().timestamp();
            let expected_block_time = genesis_timestamp + (expected_height as i64 * 600);
            let time_since_expected = now_timestamp - expected_block_time;
            let catchup_delay_threshold = 300; // 5 minutes

            // Log when next block is actually due
            if blocks_behind == 0 {
                let next_block_due = genesis_timestamp + ((current_height + 1) as i64 * 600);
                let wait_time = next_block_due - now_timestamp;
                if wait_time > 0 {
                    tracing::debug!(
                        "📅 At expected height {} - next block due in {}s at {}",
                        current_height,
                        wait_time,
                        chrono::DateTime::from_timestamp(next_block_due, 0)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    );
                }
            }

            // Smart catchup trigger:
            // - If many blocks behind (>2): Catch up immediately
            // - If 1 block behind: Use 5-minute grace period
            let should_catchup = blocks_behind > 2
                || (blocks_behind > 0 && time_since_expected >= catchup_delay_threshold);

            // Allow single-node bootstrap during initial catchup (height 0)
            // For catchup mode, allow fewer masternodes (network may be degraded)
            // For normal production, require at least 2 masternodes (reduced from 3 for network resilience)
            if masternodes.len() < 2 && current_height > 0 && !should_catchup {
                // Log periodically (every 60s) to avoid spam
                static LAST_WARN: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last_warn = LAST_WARN.load(Ordering::Relaxed);
                if now_secs - last_warn >= 60 {
                    LAST_WARN.store(now_secs, Ordering::Relaxed);
                    tracing::warn!(
                        "⚠️ Skipping normal block production: only {} masternodes active (minimum 3 required). Height: {}, Expected: {}",
                        masternodes.len(),
                        current_height,
                        expected_height
                    );
                }
                continue;
            }

            // During initial bootstrap (height 0), allow 1 masternode to produce blocks
            if masternodes.is_empty() {
                tracing::warn!("⚠️ Skipping block production: no masternodes registered");
                continue;
            }

            if should_catchup {
                // Behind schedule - trigger TSDC coordinated catchup
                tracing::info!(
                    "🧱 Catching up: height {} → {} ({} blocks behind, {}s past expected) at {} ({}:{}0) with {} eligible masternodes",
                    current_height,
                    expected_height,
                    blocks_behind,
                    time_since_expected,
                    timestamp,
                    now.hour(),
                    (now.minute() / 10),
                    masternodes.len()
                );

                // CRITICAL: When triggered by status check, skip sync attempt
                // The status check already tried sync_from_peers() and it failed
                // Retrying here creates a deadlock where we never reach production
                if !triggered_by_status_check {
                    // First, try to sync from peers
                    match block_blockchain.sync_from_peers().await {
                        Ok(()) => {
                            // Mark sync completion time
                            last_sync_time = Some(std::time::Instant::now());

                            // Re-check height after sync - only skip catchup if we actually caught up
                            let new_height = block_blockchain.get_height().await;
                            let new_expected = block_blockchain.calculate_expected_height();
                            if new_height >= new_expected {
                                tracing::info!(
                                    "✅ Sync complete - caught up to height {}",
                                    new_height
                                );
                                continue; // Actually caught up, check again next tick
                            }
                            // Sync returned Ok but we're still behind - continue to verify peers don't have longer chains
                            tracing::info!(
                                "✅ Sync complete but still {} blocks behind (height {} < expected {}) - verifying no peer has longer chain",
                                new_expected.saturating_sub(new_height),
                                new_height,
                                new_expected
                            );
                        }
                        Err(e) => {
                            tracing::warn!("⚠️  Sync from peers failed: {} - verifying no peer has longer chain", e);
                        }
                    }
                }

                // CRITICAL: Before producing catchup blocks, thoroughly verify that NO peer has a longer chain
                // This prevents creating forks when valid longer chains exist but haven't been synced yet
                let connected_peers = block_peer_registry.get_connected_peers().await;

                // If no peers connected, do NOT produce catchup blocks (prevents isolated fork)
                if connected_peers.is_empty() {
                    tracing::warn!("⚠️  No connected peers - waiting for connections before producing catchup blocks");
                    continue;
                }

                // CRITICAL: When triggered by status check, skip the peer verification wait
                // The status check already verified peers don't have longer chains
                // Skipping this prevents the 15-second delay and loop-back deadlock
                if !triggered_by_status_check {
                    // Query ALL peers for their chain heights and check if any have longer chains
                    tracing::info!(
                        "🔍 Querying {} peer(s) for chain heights before catchup",
                        connected_peers.len()
                    );

                    let mut current_height_check = block_blockchain.get_height().await;
                    let probe_start = current_height_check + 1;
                    // Cap probe at expected height - no need to request blocks that don't exist yet
                    let probe_end = expected_height;

                    // Send GetBlocks requests to all peers
                    for peer_ip in &connected_peers {
                        let msg = NetworkMessage::GetBlocks(probe_start, probe_end);
                        if let Err(e) = block_peer_registry.send_to_peer(peer_ip, msg).await {
                            tracing::warn!(
                                "Failed to query peer {} for chain height: {}",
                                peer_ip,
                                e
                            );
                        }
                    }

                    // Wait up to 15 seconds for responses and continuously check if we receive blocks
                    // This gives peers ample time to respond with their longer chains
                    tracing::info!("⏳ Waiting up to 15 seconds for peer chain responses...");
                    let wait_start = tokio::time::Instant::now();
                    let wait_duration = tokio::time::Duration::from_secs(15);
                    let mut received_blocks_from_peer = false;

                    while wait_start.elapsed() < wait_duration {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

                        let new_height = block_blockchain.get_height().await;
                        if new_height > current_height_check {
                            received_blocks_from_peer = true;
                            let new_expected = block_blockchain.calculate_expected_height();

                            tracing::info!(
                                "✅ Received blocks from peer(s): {} → {} (expected: {})",
                                current_height_check,
                                new_height,
                                new_expected
                            );

                            // If we're now at or ahead of expected height, we're caught up
                            if new_height >= new_expected {
                                tracing::info!(
                                    "✅ Fully synced to height {}, no catchup needed",
                                    new_height
                                );
                                break;
                            }

                            // Update check height and keep waiting - peer might have more blocks
                            current_height_check = new_height;
                        }
                    }

                    // If we received ANY blocks from peers, don't produce catchup blocks yet
                    // Instead, loop back and try sync_from_peers() again
                    if received_blocks_from_peer {
                        // Mark sync completion time (received blocks from peer)
                        last_sync_time = Some(std::time::Instant::now());

                        let final_height = block_blockchain.get_height().await;
                        let final_expected = block_blockchain.calculate_expected_height();

                        if final_height >= final_expected {
                            tracing::info!(
                                "✅ Sync complete after peer response, height: {}",
                                final_height
                            );
                            continue;
                        }

                        tracing::info!(
                            "📥 Received blocks from peer(s) but still behind ({} < {}) - retrying sync",
                            final_height, final_expected
                        );
                        continue; // Loop back to try sync_from_peers() again
                    }

                    tracing::info!(
                        "⏸️  No blocks received after 15s wait - peers confirmed at similar or lower height"
                    );
                } else {
                    // Triggered by status check - skip peer verification
                    tracing::info!("🚀 Triggered by status check - skipping sync/verification, proceeding directly to production");
                }

                // REVISED APPROACH: Use TSDC consensus for coordinated catchup
                //
                // Previous issue: Disabling ALL catchup meant network stalls when all nodes behind
                // Solution: Use TSDC leader election for catchup blocks
                //
                // Key principles:
                // 1. ONLY the TSDC-selected leader produces catchup blocks
                // 2. All nodes agree on leader via deterministic selection
                // 3. Non-leaders wait for leader's blocks
                // 4. This prevents competing blocks at same height (fork prevention)
                //
                // Benefits:
                // - Coordinated catchup when all nodes behind
                // - No solo block production (prevents forks)
                // - Deterministic and predictable

                // Determine catchup leader using TSDC for expected_height
                // CRITICAL: Use select_leader_for_catchup() which uses expected_height
                // for deterministic leader selection. The slot parameter is NOT used
                // (ignored internally) to ensure ALL nodes agree on the same leader
                // regardless of when they check, preventing rotating leadership deadlock.
                //
                // LEADER TIMEOUT: If the primary leader doesn't respond within timeout,
                // select a backup leader to prevent network stalls.
                let mut attempt = 0u64;

                // Check if we have a tracked leader for this height
                if let Some((tracked_leader_id, selection_time, previous_attempt)) =
                    catchup_leader_tracker.get(&expected_height)
                {
                    // Check if leader has timed out
                    if selection_time.elapsed() > leader_timeout {
                        tracing::warn!(
                            "⚠️  Catchup leader {} timed out after {:?} - selecting backup leader (attempt {})",
                            tracked_leader_id,
                            selection_time.elapsed(),
                            previous_attempt + 1
                        );
                        // Increment attempt to select next leader in rotation
                        attempt = previous_attempt + 1;
                        // Remove from tracker so we can track the new leader
                        catchup_leader_tracker.remove(&expected_height);
                    } else {
                        // Leader hasn't timed out yet, keep the same attempt
                        attempt = *previous_attempt;
                    }
                }

                // Select leader with attempt offset (0 = primary, 1+ = backups)
                let tsdc_leader = match block_tsdc
                    .select_leader_for_catchup(attempt, expected_height)
                    .await
                {
                    Ok(leader) => leader,
                    Err(e) => {
                        tracing::warn!("⚠️  Cannot select TSDC catchup leader: {} - NO ONE CAN PRODUCE BLOCKS!", e);
                        continue;
                    }
                };

                // Track this leader selection if not already tracked
                catchup_leader_tracker
                    .entry(expected_height)
                    .or_insert_with(|| {
                        (tsdc_leader.id.clone(), std::time::Instant::now(), attempt)
                    });

                // Check if we are the selected leader for this catchup operation
                let is_catchup_leader = if let Some(ref our_addr) = block_masternode_address {
                    let is_leader = tsdc_leader.id == *our_addr;
                    tracing::info!(
                        "🎲 Catchup leader selection for height {}: selected={}, we are {}, match={}",
                        expected_height,
                        tsdc_leader.id,
                        our_addr,
                        is_leader
                    );
                    is_leader
                } else {
                    tracing::debug!("Not a masternode, cannot be catchup leader");
                    false // Not a masternode, can't be leader
                };

                if !is_catchup_leader {
                    let our_desc = block_masternode_address
                        .as_deref()
                        .unwrap_or("non-masternode");

                    // Check how long we've been waiting for this leader
                    let wait_duration = if let Some((_, selection_time, _)) =
                        catchup_leader_tracker.get(&expected_height)
                    {
                        selection_time.elapsed()
                    } else {
                        std::time::Duration::from_secs(0)
                    };

                    tracing::info!(
                        "⏳ Waiting for catchup leader {} to produce blocks (we are {}, waited: {}s)",
                        tsdc_leader.id,
                        our_desc,
                        wait_duration.as_secs()
                    );

                    // Sleep briefly then check if blocks were produced
                    // This allows the leader time to produce while avoiding 10-minute wait
                    tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;

                    // After waiting, check if the leader made progress
                    let height_after_wait = block_blockchain.get_height().await;
                    if height_after_wait > current_height {
                        tracing::info!(
                            "✅ Leader {} produced block(s), height advanced: {} → {}",
                            tsdc_leader.id,
                            current_height,
                            height_after_wait
                        );
                        // Progress made - leader is working, continue to next iteration
                        continue;
                    }

                    // No progress made - check if we should rotate leader
                    if wait_duration >= leader_timeout {
                        tracing::warn!(
                            "⚠️  No progress after waiting {}s for leader {} - will rotate to backup leader",
                            wait_duration.as_secs(),
                            tsdc_leader.id
                        );
                        // The rotation will happen at the top of the next iteration
                    }

                    // Loop back to re-check leader selection (which will rotate if timed out)
                    continue;
                }

                // Check if catchup blocks are enabled in config
                if !config.node.enable_catchup_blocks {
                    tracing::warn!(
                        "⚠️  Selected as TSDC catchup leader but catchup blocks are DISABLED in config. Enable with 'enable_catchup_blocks = true' in [node] section."
                    );
                    continue;
                }

                // CRITICAL FIX: Prevent producing blocks immediately after syncing
                // When a node syncs from peers, it receives blocks but its mempool is EMPTY
                // It needs time (60s) to receive pending transactions via p2p gossip
                // Otherwise it produces blocks with no transactions (00000 merkle roots)
                if let Some(sync_time) = last_sync_time {
                    let time_since_sync = sync_time.elapsed();
                    if time_since_sync < min_time_after_sync {
                        let wait_more = min_time_after_sync - time_since_sync;
                        tracing::warn!(
                            "⏸️  Selected as catchup leader but just synced {}s ago - waiting {}s more for mempool to populate",
                            time_since_sync.as_secs(),
                            wait_more.as_secs()
                        );
                        continue;
                    }
                }

                tracing::info!(
                    "🎯 SELECTED AS CATCHUP LEADER for height {} (via TSDC consensus)",
                    expected_height
                );

                // We are the leader - produce catchup blocks with TSDC coordination
                // Acquire block production lock (P2P best practice #8)
                if is_producing
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    tracing::warn!("⚠️  Block production already in progress, skipping catchup");
                    continue;
                }

                tracing::info!(
                    "🎯 Producing {} catchup blocks as TSDC leader to reach height {}",
                    blocks_behind,
                    expected_height
                );

                // Produce catchup blocks with rate limiting
                let mut catchup_produced = 0u64;
                let genesis_timestamp = block_blockchain.genesis_timestamp();

                for target_height in (current_height + 1)..=expected_height {
                    // CRITICAL TIME COIN REQUIREMENT: Never produce blocks ahead of schedule
                    // Check current time against this block's scheduled time
                    let expected_timestamp = genesis_timestamp + (target_height as i64 * 600);
                    let now = chrono::Utc::now().timestamp();

                    if expected_timestamp > now {
                        // Haven't reached the scheduled time yet
                        let wait_seconds = expected_timestamp - now;

                        if wait_seconds > 60 {
                            // More than 1 minute until scheduled - stop catchup
                            // Regular block production will handle this at the proper time
                            tracing::info!(
                                "⏰ Stopping catchup: block {} not due for {}s",
                                target_height,
                                wait_seconds
                            );
                            break;
                        }

                        // Less than 1 minute - wait for EXACT scheduled time
                        tracing::info!(
                            "⏱️  Waiting {}s for block {} scheduled time (TIME COIN precision)",
                            wait_seconds,
                            target_height
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds as u64))
                            .await;

                        // Re-check time after waiting to ensure precision
                        let now_after_wait = chrono::Utc::now().timestamp();
                        if expected_timestamp > now_after_wait {
                            tracing::warn!(
                                "⚠️  Time check failed after wait - block {} still not due, skipping",
                                target_height
                            );
                            continue;
                        }
                    }

                    // Double-check: NEVER produce if current blockchain height >= target
                    let current_height_check = block_blockchain.get_height().await;
                    if current_height_check >= target_height {
                        tracing::info!(
                            "✓ Block {} already exists (height: {}), skipping",
                            target_height,
                            current_height_check
                        );
                        continue;
                    }

                    match block_blockchain
                        .produce_block_at_height(Some(target_height))
                        .await
                    {
                        Ok(block) => {
                            // Add block to our chain
                            if let Err(e) = block_blockchain.add_block(block.clone()).await {
                                tracing::error!("❌ Catchup block {} failed: {}", target_height, e);
                                break;
                            }

                            // Broadcast to peers
                            block_registry.broadcast_block(block.clone()).await;
                            catchup_produced += 1;

                            if catchup_produced % 10 == 0 || block.header.height == expected_height
                            {
                                tracing::info!(
                                    "📦 Catchup progress: {}/{} blocks (height: {})",
                                    catchup_produced,
                                    blocks_behind,
                                    block.header.height
                                );
                            }
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

                // Clear catchup leader tracker for completed heights
                let final_height = block_blockchain.get_height().await;
                catchup_leader_tracker.retain(|&height, _| height > final_height);

                tracing::info!(
                    "✅ Catchup complete: produced {} blocks, height now: {}",
                    catchup_produced,
                    final_height
                );
            } else {
                // Either at expected height or behind but within 5-minute grace period
                // Use normal block production - no race with catchup mode

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
                let is_producer = block_masternode_address
                    .as_ref()
                    .map(|addr| addr == &selected_producer.address)
                    .unwrap_or(false);

                if is_producer {
                    // CRITICAL: Do NOT produce blocks if we're significantly behind
                    // This prevents creating forks when out of sync
                    if blocks_behind > 10 {
                        tracing::warn!(
                            "⚠️ Skipping normal block production: {} blocks behind ({}. Expected: {}). Must sync first.",
                            blocks_behind,
                            current_height,
                            expected_height
                        );
                        continue;
                    }

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
                    if is_producing
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
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
            }

            // Handle case where we're ahead of expected height
            if current_height > expected_height {
                // Height is ahead of expected - this can happen if:
                // 1. Clock skew caused blocks to be produced early
                // 2. We received blocks from a peer with clock skew
                // Just wait silently for time to catch up - only log once per minute
                static LAST_AHEAD_LOG: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
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
    });
    shutdown_manager.register_task(block_production_handle);

    // Start network server

    println!("🌐 Starting P2P network server...");

    // Periodic status report - logs every 5 minutes at :00, :05, :10, :15, :20, :25, :30, :35, :40, :45, :50, :55
    // Also handles responsive catchup checks more frequently than 10-minute block production interval
    let status_blockchain = blockchain_server.clone();
    let status_registry = registry.clone();
    let status_tsdc_clone = tsdc_consensus.clone();
    let status_masternode_addr_clone = masternode_address.clone();
    let status_config_clone = config.clone();
    let status_catchup_trigger = catchup_trigger.clone(); // Trigger to wake up block production
    let shutdown_token_status = shutdown_token.clone();
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
                _ = shutdown_token_status.cancelled() => {
                    tracing::debug!("🛑 Status report task shutting down gracefully");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(seconds_until)) => {
                    let height = status_blockchain.get_height().await;
                    let mn_count = status_registry.list_active().await.len();

                    // Check if we need responsive catchup (between 10-minute block production checks)
                    let expected_height = status_blockchain.calculate_expected_height();
                    let blocks_behind = expected_height.saturating_sub(height);

                    if blocks_behind > 0 {
                        let genesis_timestamp = status_blockchain.genesis_timestamp();
                        let now_timestamp = chrono::Utc::now().timestamp();
                        let expected_block_time = genesis_timestamp + (expected_height as i64 * 600);
                        let time_since_expected = now_timestamp - expected_block_time;

                        // Check if catchup conditions are met (>2 blocks OR >5min past)
                        let should_catchup = blocks_behind > 2
                            || time_since_expected >= 300;

                        if should_catchup {
                            tracing::info!(
                                "📊 Status: Height={}, Active Masternodes={} | ⚠️ {} blocks behind, {}s past expected - attempting sync",
                                height,
                                mn_count,
                                blocks_behind,
                                time_since_expected
                            );

                            // Try to sync from peers first
                            match status_blockchain.sync_from_peers().await {
                                Ok(()) => {
                                    tracing::info!("✅ Responsive sync successful via 5-min check");
                                }
                                Err(_) => {
                                    // Sync failed - peers don't have blocks
                                    // Check if we should be the TSDC catchup leader
                                    // Use deterministic leader selection based only on expected_height
                                    if let Ok(tsdc_leader) = status_tsdc_clone.select_leader_for_catchup(0, expected_height).await {
                                        let is_leader = if let Some(ref our_addr) = status_masternode_addr_clone {
                                            tsdc_leader.id == *our_addr
                                        } else {
                                            false
                                        };

                                        if is_leader {
                                            // Check if catchup blocks are enabled
                                            if status_config_clone.node.enable_catchup_blocks {
                                                tracing::info!(
                                                    "🎯 We are TSDC catchup leader - triggering catchup production immediately"
                                                );
                                                // Notify block production task to run catchup immediately
                                                status_catchup_trigger.notify_one();
                                            } else {
                                                tracing::warn!(
                                                    "⚠️  TSDC catchup leader but catchup blocks DISABLED in config"
                                                );
                                            }
                                        } else {
                                            tracing::info!(
                                                "⏳ Waiting for TSDC catchup leader {} to produce blocks",
                                                tsdc_leader.id
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            tracing::info!(
                                "📊 Status: Height={}, Active Masternodes={}",
                                height,
                                mn_count
                            );
                        }
                    } else {
                        tracing::info!(
                            "📊 Status: Height={}, Active Masternodes={}",
                            height,
                            mn_count
                        );
                    }
                }
            }
        }
    });
    shutdown_manager.register_task(status_handle);

    // Prepare combined whitelist BEFORE creating server
    // This ensures masternodes are whitelisted before any connections are accepted
    let mut combined_whitelist = config.network.whitelisted_peers.clone();
    combined_whitelist.extend(discovered_peer_ips.clone());

    println!(
        "🔐 Preparing whitelist with {} trusted peer(s)...",
        combined_whitelist.len()
    );
    if !combined_whitelist.is_empty() {
        println!("  • {} from config", config.network.whitelisted_peers.len());
        println!("  • {} from time-coin.io", discovered_peer_ips.len());
    }
    println!();

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
        combined_whitelist,
        attestation_system.clone(),
    )
    .await
    {
        Ok(mut server) => {
            // NOTE: Masternodes announced via P2P are NOT auto-whitelisted.
            // Only peers from time-coin.io and config are trusted.

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

            // Share blacklist with peer connection registry for whitelist checks
            peer_connection_registry
                .set_blacklist(server.blacklist.clone())
                .await;

            println!("  ✅ Network server listening on {}", p2p_addr);

            // Phase 3 Step 3: Start sync coordinator
            let sync_coordinator_handle = blockchain.clone().spawn_sync_coordinator();
            shutdown_manager.register_task(sync_coordinator_handle);
            println!("  ✅ Sync coordinator started");

            // Start RPC server with access to blacklist
            let rpc_consensus = consensus_engine.clone();
            let rpc_utxo = utxo_mgr.clone();
            let rpc_registry = registry.clone();
            let rpc_blockchain = blockchain.clone();
            let rpc_addr_clone = rpc_addr.clone();
            let rpc_network = network_type;
            let rpc_shutdown_token = shutdown_token.clone();
            let rpc_attestation = attestation_system.clone();
            let rpc_blacklist = server.blacklist.clone();

            let rpc_handle = tokio::spawn(async move {
                match RpcServer::new(
                    &rpc_addr_clone,
                    rpc_consensus,
                    rpc_utxo,
                    rpc_network,
                    rpc_registry,
                    rpc_blockchain,
                    rpc_attestation,
                    rpc_blacklist,
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
