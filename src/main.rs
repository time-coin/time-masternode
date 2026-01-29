pub mod address;
pub mod ai;
pub mod block;
pub mod block_cache;
pub mod blockchain;
pub mod blockchain_error;
pub mod blockchain_validation;
pub mod config;
pub mod consensus;
pub mod constants;
pub mod crypto;
pub mod error;
pub mod finality_proof;
pub mod masternode_authority;
pub mod masternode_registry;
pub mod network;
pub mod network_type;
pub mod peer_manager;
pub mod rpc;
pub mod shutdown;
pub mod state_notifier;
pub mod storage;
pub mod time_sync;
pub mod timevote;
pub mod transaction_pool;
pub mod transaction_priority;
pub mod transaction_selection;
pub mod timelock;
pub mod tx_index;
pub mod types;
pub mod utxo_manager;
pub mod wallet;

use blockchain::Blockchain;
use chrono::Timelike;
use clap::Parser;
use config::Config;
use consensus::ConsensusEngine;
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
use timelock::TSCDConsensus;
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

        let masternode = types::Masternode::new_legacy(
            ip_only,
            wallet_address.clone(),
            tier.collateral(),
            *wallet.public_key(),
            tier,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );

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

    // Initialize UTXO states from storage
    tracing::info!("🔧 Initializing UTXO state manager from storage...");
    if let Err(e) = utxo_mgr.initialize_states().await {
        eprintln!("⚠️ Warning: Failed to initialize UTXO states: {}", e);
    }

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

    println!("✓ Ready to process transactions\n");

    // Initialize ConsensusEngine with direct reference to masternode registry
    let mut consensus_engine = ConsensusEngine::new(Arc::clone(&registry), utxo_mgr.clone());

    // Enable AI validation using the same db as block storage
    consensus_engine.enable_ai_validation(Arc::new(block_storage.clone()));

    let consensus_engine = Arc::new(consensus_engine);
    tracing::info!("✓ Consensus engine initialized with AI validation");

    // Initialize TSDC consensus engine with masternode registry
    let tsdc_consensus =
        TSCDConsensus::with_masternode_registry(Default::default(), registry.clone());
    tracing::info!("✓ TSDC consensus engine initialized");

    let tsdc_consensus = Arc::new(tsdc_consensus);

    // Initialize blockchain
    let mut blockchain = Blockchain::new(
        block_storage,
        consensus_engine.clone(),
        registry.clone(),
        utxo_mgr.clone(),
        network_type,
    );

    // Initialize transaction index for O(1) lookups
    tracing::info!("🔧 Initializing transaction index...");
    let tx_index_path = format!("{}/txindex", db_dir);
    let tx_index = match tx_index::TransactionIndex::new(&tx_index_path) {
        Ok(idx) => {
            let tx_index_arc = Arc::new(idx);
            blockchain.set_tx_index(tx_index_arc.clone());
            Some(tx_index_arc)
        }
        Err(e) => {
            tracing::warn!("Failed to initialize transaction index: {}", e);
            tracing::warn!("Transaction lookups will use slower blockchain scan");
            None
        }
    };

    let blockchain = Arc::new(blockchain);

    // Build transaction index if it exists and is empty
    if let Some(ref idx) = tx_index {
        if idx.is_empty() && blockchain.get_height() > 0 {
            tracing::info!("📊 Building transaction index from blockchain...");
            if let Err(e) = blockchain.build_tx_index().await {
                tracing::warn!("Failed to build transaction index: {}", e);
            }
        } else {
            tracing::info!(
                "✅ Transaction index ready: {} transactions indexed",
                idx.len()
            );
        }
    }

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

    // Create sync completion notifier for masternode announcement
    let sync_complete = Arc::new(tokio::sync::Notify::new());

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

                // Set signing key for consensus engine
                use ed25519_dalek::SigningKey;
                use rand::rngs::OsRng;
                let mut csprng = OsRng;
                let signing_key = SigningKey::from_bytes(&rand::Rng::gen(&mut csprng));
                if let Err(e) =
                    consensus_engine.set_identity(mn.address.clone(), signing_key.clone())
                {
                    eprintln!("⚠️ Failed to set consensus identity: {}", e);
                }

                tracing::info!("✓ Registered masternode: {}", mn.wallet_address);
                tracing::info!("✓ Consensus engine identity configured");

                // Broadcast masternode announcement will happen after initial sync completes
                // (see announcement task below)
            }
            Err(e) => {
                tracing::error!("❌ Failed to register masternode: {}", e);
                std::process::exit(1);
            }
        }

        // Start peer exchange task (for masternode discovery)
        let peer_connection_registry_clone = peer_connection_registry.clone();
        let shutdown_token_clone = shutdown_token.clone();
        let peer_exchange_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = shutdown_token_clone.cancelled() => {
                        tracing::debug!("🛑 Peer exchange task shutting down gracefully");
                        break;
                    }
                    _ = interval.tick() => {
                        // Request masternodes from all connected peers for peer exchange
                        tracing::info!("📤 Broadcasting GetMasternodes to all peers");
                        peer_connection_registry_clone
                            .broadcast(NetworkMessage::GetMasternodes)
                            .await;
                    }
                }
            }
        });
        shutdown_manager.register_task(peer_exchange_handle);

        // Start masternode announcement task (waits for sync to complete)
        let mn_for_announcement = mn.clone();
        let peer_registry_for_announcement = peer_connection_registry.clone();
        let sync_complete_wait = sync_complete.clone();
        let announcement_handle = tokio::spawn(async move {
            tracing::info!(
                "⏳ Waiting for blockchain sync to complete before announcing masternode..."
            );

            // Wait for sync completion signal
            sync_complete_wait.notified().await;

            // Sync complete - now broadcast announcement
            let announcement = NetworkMessage::MasternodeAnnouncement {
                address: mn_for_announcement.address.clone(),
                reward_address: mn_for_announcement.wallet_address.clone(),
                tier: mn_for_announcement.tier,
                public_key: mn_for_announcement.public_key,
            };

            peer_registry_for_announcement.broadcast(announcement).await;
            tracing::info!("📢 Broadcast masternode announcement to network (after sync complete)");
        });
        shutdown_manager.register_task(announcement_handle);
    }

    // Initialize blockchain and sync from peers in background
    let blockchain_init = blockchain.clone();
    let blockchain_server = blockchain_init.clone();
    let peer_registry_for_sync = peer_connection_registry.clone();
    let sync_complete_signal = sync_complete.clone();

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
        let has_genesis = blockchain_init.get_height() > 0
            || blockchain_init.get_block_by_height(0).await.is_ok();

        if !has_genesis {
            tracing::error!("❌ Failed to load genesis block - cannot proceed");
            return;
        }

        tracing::info!("✓ Genesis block loaded, now syncing remaining blocks from peers");

        // STEP 2: Wait for peer connections to sync remaining blocks (reduced for faster startup)
        let mut wait_seconds = 0u64;
        let max_wait = 20u64; // Reduced from 60s - start syncing as soon as peers connect
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

        // STEP 3: Start fork detection BEFORE syncing (run immediately then every 15 seconds for immediate sync)
        Blockchain::start_chain_comparison_task(blockchain_init.clone());
        tracing::info!("✓ Fork detection task started (checks immediately, then every 15 seconds)");

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
        if let Err(e) = blockchain_init.sync_from_peers(None).await {
            tracing::warn!("⚠️  Initial sync from peers: {}", e);
        }

        // Verify chain integrity and download any missing blocks
        if let Err(e) = blockchain_init.ensure_chain_complete().await {
            tracing::warn!("⚠️  Chain integrity check: {}", e);
        }

        // Continue syncing if still behind
        if let Err(e) = blockchain_init.sync_from_peers(None).await {
            tracing::warn!("⚠️  Block sync from peers: {}", e);
        }

        // Initial sync complete - signal masternode announcement can proceed
        tracing::info!("✅ Initial blockchain sync complete");
        sync_complete_signal.notify_one();

        // Start periodic genesis validation check (in case of late genesis file deployment)
        let blockchain_for_genesis = blockchain_init.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

                // Only check if we don't have a valid genesis yet
                let height = blockchain_for_genesis.get_height();
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
    let block_masternode_address = masternode_address.clone(); // For leader comparison
    let shutdown_token_block = shutdown_token.clone();
    let block_consensus_engine = consensus_engine.clone(); // For TSDC voting

    // Guard flag to prevent duplicate block production (P2P best practice #8)
    let is_producing_block = Arc::new(AtomicBool::new(false));
    let is_producing_block_clone = is_producing_block.clone();

    // Trigger for immediate catchup block production (when 5-min status check detects need)
    let catchup_trigger = Arc::new(tokio::sync::Notify::new());
    let catchup_trigger_producer = catchup_trigger.clone();

    let block_production_handle = tokio::spawn(async move {
        let is_producing = is_producing_block_clone;

        // Give time for initial blockchain sync to complete before starting block production
        // Reduced to 10s for faster catchup startup
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

        // Time-based catchup trigger: Check if we're behind schedule
        // Use time rather than block count to determine when to trigger catchup
        let current_height = block_blockchain.get_height();
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
                "⚡ {} blocks behind - starting immediate TimeLock catchup (>2 blocks threshold)",
                blocks_behind
            );
            0
        } else if blocks_behind > 0 && time_since_expected >= catchup_delay_threshold {
            // 1-2 blocks behind AND 5+ minutes past when block should have been produced
            // Start catchup immediately - normal production had its chance
            tracing::info!(
                "⚡ {} blocks behind, {}s past expected block time - starting immediate TimeLock catchup",
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

        // Use a short interval (1 second) and check timing internally
        // This allows rapid catchup when behind while still respecting 10-minute marks when synced
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_block_period_started: u64 = 0; // Track which block period we've started

        // Leader rotation timeout tracking
        // If a leader doesn't produce within LEADER_TIMEOUT_SECS, rotate to next leader
        const LEADER_TIMEOUT_SECS: u64 = 10; // Wait 10s before rotating to backup leader (2x block production time)
        let mut waiting_for_height: Option<u64> = None;
        let mut waiting_since: Option<std::time::Instant> = None;
        let mut leader_attempt: u64 = 0; // Increments when leader times out

        loop {
            tokio::select! {
                _ = shutdown_token_block.cancelled() => {
                    tracing::debug!("🛑 Block production task shutting down gracefully");
                    break;
                }
                _ = catchup_trigger_producer.notified() => {
                    // Triggered by status check - immediate check
                    tracing::info!("🔔 Catchup production triggered by status check");
                }
                _ = interval.tick() => {
                    // Regular 1-second check
                }
            }

            // Mark start of new block period (only once per period)
            let current_height = block_blockchain.get_height();
            let expected_period = current_height + 1;
            if expected_period > last_block_period_started {
                block_registry.start_new_block_period().await;
                last_block_period_started = expected_period;
            }

            let expected_height = block_blockchain.calculate_expected_height();

            // Get masternodes eligible for leader selection and rewards
            // CRITICAL: This MUST use the SAME logic as blockchain.rs produce_block_at_height()
            // to ensure selected leader is eligible for rewards (prevents participation chain break)
            let blocks_behind = expected_height.saturating_sub(current_height);
            let is_bootstrap = current_height <= 3;
            // During deep catchup, use all active masternodes (participation bitmap may be corrupted from fork)
            let is_deep_catchup = blocks_behind >= 50;

            let eligible = if is_bootstrap || is_deep_catchup {
                // Bootstrap mode (height 0-3) OR deep catchup: use all active masternodes
                if is_bootstrap {
                    tracing::debug!(
                        "🌱 Bootstrap mode (height {}): using all active masternodes",
                        current_height
                    );
                } else {
                    tracing::info!(
                        "🔄 Deep catchup mode ({} blocks behind): using all active masternodes (bypassing potentially corrupted bitmap)",
                        blocks_behind
                    );
                }
                block_registry.get_eligible_for_rewards().await
            } else {
                // Normal/catchup mode (height > 3): use participation-based selection
                // This matches blockchain.rs get_masternodes_for_rewards() logic
                let prev_block = block_blockchain
                    .get_block_by_height(current_height)
                    .await
                    .ok();

                if let Some(prev_block) = prev_block {
                    // Extract active masternodes using previous block's bitmap
                    let active_infos = block_registry
                        .get_active_from_bitmap(&prev_block.header.active_masternodes_bitmap)
                        .await;

                    // Fallback: If bitmap is empty (legacy blocks or no voters), use all active masternodes
                    if active_infos.is_empty() {
                        tracing::warn!(
                            "⚠️  Previous block has empty bitmap (legacy block or no voters) - falling back to all active masternodes"
                        );
                        block_registry.get_eligible_for_rewards().await
                    } else {
                        tracing::debug!(
                            "📊 Using {} active masternodes from previous block's bitmap",
                            active_infos.len()
                        );

                        active_infos
                            .into_iter()
                            .map(|info| (info.masternode, info.reward_address))
                            .collect()
                    }
                } else {
                    // Can't get previous block - fallback to all active
                    tracing::warn!(
                        "⚠️  Cannot get previous block {} - falling back to all active masternodes",
                        current_height
                    );
                    block_registry.get_eligible_for_rewards().await
                }
            };

            let mut masternodes: Vec<Masternode> =
                eligible.iter().map(|(mn, _)| mn.clone()).collect();
            // Sort deterministically by address for consistent leader election across all nodes
            sort_masternodes_canonical(&mut masternodes);

            // Calculate time-based values for block production
            let genesis_timestamp = block_blockchain.genesis_timestamp();
            let now_timestamp = chrono::Utc::now().timestamp();

            // Require minimum masternodes for production
            // Always enforce minimum 3 masternodes for block production
            if masternodes.len() < 3 {
                // Log periodically (every 60s) to avoid spam
                static LAST_WARN: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last_warn = LAST_WARN.load(Ordering::Relaxed);
                if now_secs - last_warn >= 60 {
                    LAST_WARN.store(now_secs, Ordering::Relaxed);
                    tracing::warn!(
                        "⚠️ Skipping block production: only {} masternodes active (minimum 3 required). Height: {}, Expected: {}",
                        masternodes.len(),
                        current_height,
                        expected_height
                    );
                }
                continue;
            }

            // Check we have masternodes
            if masternodes.is_empty() {
                // Log periodically (every 60s) to avoid spam
                static LAST_EMPTY_WARN: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last_warn = LAST_EMPTY_WARN.load(Ordering::Relaxed);
                if now_secs - last_warn >= 60 {
                    LAST_EMPTY_WARN.store(now_secs, Ordering::Relaxed);
                    tracing::warn!(
                        "⚠️ Skipping block production: no masternodes registered. Height: {}, Expected: {}, Behind: {}",
                        current_height, expected_height, blocks_behind
                    );
                }
                continue;
            }

            // ═══════════════════════════════════════════════════════════════════════════════
            // UNIFIED BLOCK PRODUCTION - All nodes move forward together
            // ═══════════════════════════════════════════════════════════════════════════════
            //
            // Single production mode with these rules:
            // 1. If at expected height: wait for next scheduled time
            // 2. If behind by 1+ blocks and 60s past scheduled: produce the block
            // 3. If way behind (network was down): sync first, then produce together
            // 4. Minority nodes that won't sync don't block majority progress
            // 5. Use TSDC/timevote consensus for leader election
            // ═══════════════════════════════════════════════════════════════════════════════

            let next_height = current_height + 1;
            let next_block_scheduled_time = genesis_timestamp + (next_height as i64 * 600); // 600 seconds (10 min) per block
            let time_past_scheduled = now_timestamp - next_block_scheduled_time;

            // Grace period: 60 seconds after scheduled time before we produce
            const GRACE_PERIOD_SECS: i64 = 60;

            // Sync threshold: if more than this many blocks behind, try to sync first
            const SYNC_THRESHOLD_BLOCKS: u64 = 5;

            // Case 1: Next block not due yet (more than grace period away) - wait
            // BUT: Skip this check during catchup mode (when far behind)
            if time_past_scheduled < -GRACE_PERIOD_SECS && blocks_behind < SYNC_THRESHOLD_BLOCKS {
                let wait_secs = (-time_past_scheduled) - GRACE_PERIOD_SECS;
                tracing::debug!(
                    "📅 Block {} not due for {}s (will produce at scheduled + {}s grace)",
                    next_height,
                    wait_secs,
                    GRACE_PERIOD_SECS
                );
                continue;
            }

            // Case 2: Way behind - try to sync first before producing
            // BUT: Check if we're in a bootstrap scenario (everyone at same height)
            if blocks_behind >= SYNC_THRESHOLD_BLOCKS {
                tracing::info!(
                    "🔄 {} blocks behind - checking if peers have blocks to sync",
                    blocks_behind
                );

                // Use only compatible peers for sync (excludes nodes on incompatible chains)
                let connected_peers = block_peer_registry.get_compatible_peers().await;

                if !connected_peers.is_empty() {
                    // First, check consensus: are all peers at the same height as us?
                    // This detects bootstrap scenarios where no one has produced blocks yet
                    let min_peers_for_check = connected_peers.len().min(3);
                    if connected_peers.len() >= min_peers_for_check {
                        if let Some((consensus_height, _)) =
                            block_blockchain.compare_chain_with_peers().await
                        {
                            // If consensus height equals our height, no one has blocks ahead
                            // This is a bootstrap scenario - proceed to produce instead of sync
                            if consensus_height == current_height {
                                tracing::info!(
                                    "✅ Bootstrap detected: {} peers agree at height {} - proceeding to block production",
                                    connected_peers.len(),
                                    current_height
                                );
                                // Fall through to production logic below
                            } else {
                                // Consensus is ahead of us - try to sync
                                tracing::info!(
                                    "📥 Peers have blocks up to height {} - attempting sync",
                                    consensus_height
                                );

                                // Request blocks from peers
                                let probe_start = current_height + 1;
                                let probe_end = consensus_height.min(current_height + 50);

                                tracing::info!(
                                    "📤 Requesting blocks {}-{} from {} peer(s)",
                                    probe_start,
                                    probe_end,
                                    connected_peers.len()
                                );

                                for peer_ip in &connected_peers {
                                    let msg = NetworkMessage::GetBlocks(probe_start, probe_end);
                                    let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                                }

                                // Wait briefly for sync responses
                                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                                // Check if we synced
                                let new_height = block_blockchain.get_height();
                                if new_height > current_height {
                                    tracing::info!(
                                        "✅ Synced {} blocks: {} → {}",
                                        new_height - current_height,
                                        current_height,
                                        new_height
                                    );
                                    // Loop back to re-evaluate with new height
                                    continue;
                                }

                                // No sync progress - peers don't have blocks either
                                // Fall through to produce the block ourselves (majority moves forward)
                                tracing::warn!(
                                    "⚠️  No sync progress - peers may not have blocks. Majority will produce."
                                );
                            }
                        } else {
                            // No consensus response - try blind sync anyway
                            tracing::warn!(
                                "⚠️  No consensus response from peers - attempting blind sync"
                            );

                            let probe_start = current_height + 1;
                            let probe_end = expected_height.min(current_height + 50);

                            for peer_ip in &connected_peers {
                                let msg = NetworkMessage::GetBlocks(probe_start, probe_end);
                                let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                            }

                            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                            let new_height = block_blockchain.get_height();
                            if new_height > current_height {
                                continue;
                            }

                            tracing::warn!("⚠️  No sync progress - proceeding to production");
                        }
                    }
                } else {
                    tracing::warn!("⚠️  No peers available for sync - proceeding to production");
                }
            }

            // Case 3: Within grace period or sync failed - time to produce
            // Use TSDC consensus for leader election

            // First: Verify we're on the consensus chain (prevent fork perpetuation)
            // Use compatible peers only (excludes nodes on incompatible chains like old software)
            let connected_peers = block_peer_registry.get_compatible_peers().await;
            let min_peers_for_consensus = (masternodes.len() / 2).max(2); // Majority or at least 2

            if connected_peers.len() >= min_peers_for_consensus {
                // Check if we're on the same chain as majority
                if let Some((consensus_height, _)) =
                    block_blockchain.compare_chain_with_peers().await
                {
                    if consensus_height == current_height {
                        // Same height but different hash - we're on minority chain
                        tracing::warn!(
                            "🔀 Fork detected at height {}: syncing to majority chain before producing",
                            current_height
                        );
                        if let Err(e) = block_blockchain.sync_from_peers(None).await {
                            tracing::warn!("⚠️  Sync to majority failed: {}", e);
                        }
                        continue;
                    }
                }
            }

            // Get previous block hash for leader selection
            let prev_block_hash = match block_blockchain.get_block_hash(current_height) {
                Ok(hash) => hash,
                Err(e) => {
                    tracing::error!("Failed to get previous block hash: {}", e);
                    continue;
                }
            };

            // Leader rotation timeout tracking
            // Reset attempt counter when we move to a new height
            if waiting_for_height != Some(next_height) {
                waiting_for_height = Some(next_height);
                waiting_since = Some(std::time::Instant::now());
                leader_attempt = 0;
            } else if let Some(since) = waiting_since {
                // Check if we've been waiting too long for this height
                let elapsed = since.elapsed().as_secs();
                let expected_attempt = elapsed / LEADER_TIMEOUT_SECS;
                if expected_attempt > leader_attempt {
                    leader_attempt = expected_attempt;
                    tracing::warn!(
                        "⏱️  Leader timeout for block {} ({}s elapsed) - rotating to backup leader (attempt {})",
                        next_height,
                        elapsed,
                        leader_attempt
                    );
                }
            }

            // Deterministic leader selection using TSDC with tier-based weighting
            // Hash(prev_block_hash || next_height || attempt) determines the leader
            // Higher tiers get selected more frequently based on reward_weight()
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(prev_block_hash);
            hasher.update(next_height.to_le_bytes());
            hasher.update(leader_attempt.to_le_bytes()); // Include attempt for leader rotation
            let selection_hash: [u8; 32] = hasher.finalize().into();

            // Build cumulative weight array for weighted selection
            // Each masternode's weight = tier.reward_weight()
            let mut cumulative_weights: Vec<u64> = Vec::with_capacity(masternodes.len());
            let mut total_weight = 0u64;
            for mn in &masternodes {
                total_weight = total_weight.saturating_add(mn.tier.reward_weight());
                cumulative_weights.push(total_weight);
            }

            // Convert hash to random value in range [0, total_weight)
            let random_value = {
                let mut val = 0u64;
                for (i, &byte) in selection_hash.iter().take(8).enumerate() {
                    val |= (byte as u64) << (i * 8);
                }
                val % total_weight
            };

            // Binary search to find selected masternode based on weight
            let producer_index = cumulative_weights
                .iter()
                .position(|&w| random_value < w)
                .unwrap_or(masternodes.len() - 1);

            let selected_producer = &masternodes[producer_index];
            let is_producer = block_masternode_address
                .as_ref()
                .map(|addr| addr == &selected_producer.address)
                .unwrap_or(false);

            // Log leader selection at INFO level every 30 seconds to help debug production issues
            static LAST_LEADER_LOG: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = chrono::Utc::now().timestamp();
            let last_log = LAST_LEADER_LOG.load(Ordering::Relaxed);
            if now_secs - last_log >= 30 || leader_attempt > 0 {
                LAST_LEADER_LOG.store(now_secs, Ordering::Relaxed);
                tracing::info!(
                    "🎲 Block {} leader selection: {} of {} masternodes, selected: {} (us: {}){}",
                    next_height,
                    producer_index + 1,
                    masternodes.len(),
                    selected_producer.address,
                    if is_producer { "YES" } else { "NO" },
                    if leader_attempt > 0 {
                        format!(" [attempt {}]", leader_attempt)
                    } else {
                        String::new()
                    }
                );
            }

            if !is_producer {
                tracing::debug!(
                    "⏸️  Not selected for block {} (producer: {}, attempt: {})",
                    next_height,
                    selected_producer.address,
                    leader_attempt
                );
                continue;
            }

            // We are the selected producer!
            tracing::info!(
                "🎯 Selected as block producer for height {} ({}s past scheduled time)",
                next_height,
                time_past_scheduled
            );

            // Safety checks before producing
            // Always require at least 3 peers to prevent isolated nodes from creating forks
            // Even during catchup, we need network consensus to produce valid blocks
            let min_peers_required = 3;
            if connected_peers.len() < min_peers_required {
                tracing::warn!(
                    "⚠️ Only {} peer(s) connected - waiting for more peers before producing",
                    connected_peers.len()
                );
                continue;
            }

            // CRITICAL: Check if block already exists in chain
            // This prevents producing a block that's already finalized
            // Note: We don't check the cache because proposals may timeout/fail
            // and we need to allow retry. TSDC consensus voting prevents duplicates.
            if block_blockchain.get_height() >= next_height {
                tracing::debug!(
                    "⏭️  Block {} already exists in chain (height {}), skipping production",
                    next_height,
                    block_blockchain.get_height()
                );
                continue;
            }

            // Acquire block production lock
            if is_producing
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                tracing::warn!("⚠️  Block production already in progress, skipping");
                continue;
            }

            // Produce the block
            match block_blockchain
                .produce_block_at_height(
                    None,
                    Some(selected_producer.wallet_address.clone()),
                    Some(selected_producer.address.clone()),
                )
                .await
            {
                Ok(block) => {
                    let block_height = block.header.height;
                    let block_hash = block.hash();

                    tracing::info!(
                        "📦 Block {} produced: {} txs, {} rewards - broadcasting for consensus",
                        block_height,
                        block.transactions.len(),
                        block.masternode_rewards.len()
                    );

                    // TSDC Consensus Flow:
                    // 1. Cache block locally for finalization
                    // 2. Broadcast TimeLockBlockProposal to all peers (NOT add to chain yet)
                    // 3. All nodes (including us) validate and vote
                    // 4. When >50% prepare votes → precommit phase
                    // 5. When >50% precommit votes → block finalized, all add to chain

                    // Step 1: Cache the block for finalization (leader must also cache)
                    let (_, block_cache_opt, _) = block_peer_registry.get_tsdc_resources().await;
                    if let Some(cache) = &block_cache_opt {
                        cache.insert(block_hash, block.clone());
                        tracing::debug!("💾 Leader cached block {} for consensus", block_height);
                    }

                    // Step 2: Broadcast proposal to all peers
                    let proposal = crate::network::message::NetworkMessage::TimeLockBlockProposal {
                        block: block.clone(),
                    };
                    block_peer_registry.broadcast(proposal).await;

                    tracing::info!(
                        "📤 TimeLockBlockProposal broadcast for block {} (hash: {}...)",
                        block_height,
                        hex::encode(&block_hash[..4])
                    );

                    // Step 3: Generate our own prepare vote (leader participates in voting)
                    if let Some(ref our_addr) = block_masternode_address {
                        // Look up our weight from masternode registry
                        let our_weight = match block_registry.get(our_addr).await {
                            Some(info) => info.masternode.collateral.max(1),
                            None => 1u64,
                        };

                        // Record our prepare vote in consensus engine
                        block_consensus_engine.timevote.accumulate_prepare_vote(
                            block_hash,
                            our_addr.clone(),
                            our_weight,
                        );

                        // Broadcast our prepare vote
                        // Sign the vote with our masternode key
                        let signature =
                            if let Some(signing_key) = block_consensus_engine.get_signing_key() {
                                use ed25519_dalek::Signer;
                                let mut msg = Vec::new();
                                msg.extend_from_slice(&block_hash);
                                msg.extend_from_slice(our_addr.as_bytes());
                                msg.extend_from_slice(b"PREPARE"); // Vote type
                                signing_key.sign(&msg).to_bytes().to_vec()
                            } else {
                                tracing::warn!("⚠️ No signing key available for prepare vote");
                                vec![]
                            };

                        let vote = crate::network::message::NetworkMessage::TimeVotePrepare {
                            block_hash,
                            voter_id: our_addr.clone(),
                            signature,
                        };
                        block_peer_registry.broadcast(vote).await;

                        tracing::info!(
                            "🗳️  Cast prepare vote for block {} (our weight: {})",
                            block_height,
                            our_weight
                        );
                    }

                    // Step 4: Wait for consensus to finalize the block
                    // The message_handler will add the block when precommit consensus is reached
                    // During catchup, check if peers have synced instead of waiting for timeout

                    let consensus_timeout = if blocks_behind > 0 {
                        std::time::Duration::from_secs(10) // Catchup: 10 second max wait
                    } else {
                        std::time::Duration::from_secs(30) // Normal: 30 second max wait
                    };
                    let consensus_start = std::time::Instant::now();
                    let check_interval = std::time::Duration::from_millis(100); // Check frequently

                    loop {
                        tokio::time::sleep(check_interval).await;

                        // Check if block was added (consensus reached via message handler)
                        let new_height = block_blockchain.get_height();
                        if new_height >= block_height {
                            tracing::info!("✅ Block {} finalized via consensus!", block_height);
                            break;
                        }

                        // During catchup: Check if most peers have synced to this height
                        // This is much faster than waiting for full consensus timeout
                        if blocks_behind > 0 && consensus_start.elapsed().as_millis() > 500 {
                            let connected_peers = block_peer_registry.get_connected_peers().await;
                            if !connected_peers.is_empty() {
                                let mut synced_count = 0;
                                let mut checked_count = 0;

                                for peer_ip in &connected_peers {
                                    if let Some(peer_height) =
                                        block_peer_registry.get_peer_height(peer_ip).await
                                    {
                                        checked_count += 1;
                                        if peer_height >= block_height {
                                            synced_count += 1;
                                        }
                                    }
                                }

                                // If majority of reachable peers have synced, move on
                                // This allows fast catchup without waiting for full consensus
                                if checked_count > 0 {
                                    let sync_percentage =
                                        (synced_count as f64 / checked_count as f64) * 100.0;

                                    if synced_count >= (checked_count * 2 / 3) && synced_count >= 2
                                    {
                                        tracing::info!(
                                            "⚡ Block {} catchup: {}/{} peers synced ({:.0}%) - continuing",
                                            block_height,
                                            synced_count,
                                            checked_count,
                                            sync_percentage
                                        );
                                        break;
                                    }

                                    // Log sync progress every 2 seconds
                                    if consensus_start.elapsed().as_secs() % 2 == 0
                                        && consensus_start.elapsed().as_millis() < 300
                                    {
                                        tracing::debug!(
                                            "🔄 Block {} sync: {}/{} peers at height ({:.0}%)",
                                            block_height,
                                            synced_count,
                                            checked_count,
                                            sync_percentage
                                        );
                                    }
                                }
                            }
                        }

                        // Log consensus progress periodically (normal mode or if peers not responding)
                        let prepare_weight = block_consensus_engine
                            .timevote
                            .get_prepare_weight(block_hash);
                        let precommit_weight = block_consensus_engine
                            .timevote
                            .get_precommit_weight(block_hash);

                        if consensus_start.elapsed().as_secs() % 5 == 0
                            && consensus_start.elapsed().as_millis() < 600
                        {
                            tracing::debug!(
                                "🗳️  Consensus progress for block {}: prepare={}, precommit={}",
                                block_height,
                                prepare_weight,
                                precommit_weight
                            );
                        }

                        // Check timeout
                        if consensus_start.elapsed() > consensus_timeout {
                            tracing::warn!(
                                "⏰ Consensus timeout for block {} after {}s (prepare={}, precommit={})",
                                block_height,
                                consensus_timeout.as_secs(),
                                prepare_weight,
                                precommit_weight
                            );

                            // Fallback: If we're the leader, add block when:
                            // 1. We have SOME votes (partial consensus), OR
                            // 2. There are very few validators (network bootstrap/recovery)
                            // This prevents network stall when peers are slow/offline
                            let validator_count =
                                block_consensus_engine.timevote.get_validators().len();
                            let should_fallback = prepare_weight > 0
                                || validator_count <= 2
                                || (validator_count > 0 && prepare_weight == 0);

                            if should_fallback {
                                tracing::warn!(
                                    "⚡ Fallback: Adding block {} (prepare_weight={}, validators={})",
                                    block_height,
                                    prepare_weight,
                                    validator_count
                                );
                                if let Err(e) = block_blockchain.add_block(block.clone()).await {
                                    tracing::error!("❌ Failed to add block in fallback: {}", e);
                                } else {
                                    // Broadcast the finalized block for late-joining nodes
                                    let finalized_msg =
                                        crate::network::message::NetworkMessage::TimeLockBlockProposal {
                                            block: block.clone(),
                                        };
                                    block_peer_registry.broadcast(finalized_msg).await;
                                    tracing::info!(
                                        "✅ Block {} added via fallback, broadcast to peers",
                                        block_height
                                    );
                                }
                            } else {
                                tracing::error!(
                                    "❌ Cannot add block {}: no votes and too many validators ({})",
                                    block_height,
                                    validator_count
                                );
                            }

                            // Clear consensus state for this block
                            block_consensus_engine
                                .timevote
                                .cleanup_block_votes(block_hash);
                            break;
                        }
                    }

                    // Check if we're still behind and need to continue immediately
                    let new_height = block_blockchain.get_height();
                    let new_expected = block_blockchain.calculate_expected_height();
                    let still_behind = new_expected.saturating_sub(new_height);
                    if still_behind > 0 {
                        tracing::info!(
                            "🔄 Still {} blocks behind expected height {}, continuing catchup",
                            still_behind,
                            new_expected
                        );
                        is_producing.store(false, Ordering::SeqCst);
                        interval.reset(); // Reset interval to avoid double-tick
                        continue; // Skip waiting, loop immediately
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to produce block: {}", e);
                }
            }

            is_producing.store(false, Ordering::SeqCst);
        }
    });
    shutdown_manager.register_task(block_production_handle);

    // Start network server

    println!("🌐 Starting P2P network server...");

    // Periodic status report - logs every 1 minute for immediate sync detection
    // Also handles responsive catchup checks more frequently than 10-minute block production interval
    let status_blockchain = blockchain_server.clone();
    let status_registry = registry.clone();
    let status_tsdc_clone = tsdc_consensus.clone();
    let status_masternode_addr_clone = masternode_address.clone();
    let status_config_clone = config.clone();
    let status_catchup_trigger = catchup_trigger.clone(); // Trigger to wake up block production
    let shutdown_token_status = shutdown_token.clone();
    let status_handle = tokio::spawn(async move {
        let mut tick_count = 0u64; // Track ticks for cache monitoring
        loop {
            // Check every 60 seconds for immediate sync response
            tokio::select! {
                _ = shutdown_token_status.cancelled() => {
                    tracing::debug!("🛑 Status report task shutting down gracefully");
                    break;
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {
                    tick_count += 1;

                    let height = status_blockchain.get_height();
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
                            let registered_count = status_registry.total_count().await;
                            tracing::warn!(
                                "📊 ═══════════════════════════════════════════════════════════════",
                            );
                            tracing::warn!(
                                "📊 NODE STATUS | Height: {} | Masternodes: {} active / {} registered | ⚠️ {} BLOCKS BEHIND",
                                height,
                                mn_count,
                                registered_count,
                                blocks_behind
                            );
                            tracing::warn!(
                                "📊 Sync Status: {}s past expected block time - attempting sync",
                                time_since_expected
                            );
                            tracing::warn!(
                                "📊 ═══════════════════════════════════════════════════════════════",
                            );

                            // Try to sync from peers first
                            match status_blockchain.sync_from_peers(None).await {
                                Ok(()) => {
                                    tracing::info!("✅ Responsive sync successful via 5-min check");
                                }
                                Err(_) => {
                                    // Sync failed - peers don't have blocks
                                    // Check if we should be the TimeLock catchup leader
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
                                                    "🎯 We are TimeLock catchup leader - triggering catchup production immediately"
                                                );
                                                // Notify block production task to run catchup immediately
                                                status_catchup_trigger.notify_one();
                                            } else {
                                                tracing::warn!(
                                                    "⚠️  TimeLock catchup leader but catchup blocks DISABLED in config"
                                                );
                                            }
                                        } else {
                                            tracing::info!(
                                                "⏳ Waiting for TimeLock catchup leader {} to produce blocks",
                                                tsdc_leader.id
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            let registered_count = status_registry.total_count().await;
                            tracing::warn!(
                                "📊 ═══════════════════════════════════════════════════════════════",
                            );
                            tracing::warn!(
                                "📊 NODE STATUS | Height: {} | Masternodes: {} active / {} registered | ✅ ON TRACK",
                                height,
                                mn_count,
                                registered_count
                            );
                            tracing::warn!(
                                "📊 ═══════════════════════════════════════════════════════════════",
                            );

                            // Log cache statistics every 5 checks (every ~25 minutes)
                            if tick_count % 5 == 0 && tick_count > 0 {
                                let cache_stats = status_blockchain.get_cache_stats();
                                let cache_memory_mb = status_blockchain.get_cache_memory_usage() / (1024 * 1024);
                                tracing::info!(
                                    "💾 Block Cache: {} | Memory: {}MB",
                                    cache_stats,
                                    cache_memory_mb
                                );
                            }
                        }
                    } else {
                        tracing::info!(
                            "📊 Status: Height={}, Active Masternodes={}",
                            height,
                            mn_count
                        );

                        // Log cache statistics every 5 checks (every ~25 minutes)
                        if tick_count % 5 == 0 && tick_count > 0 {
                            let cache_stats = status_blockchain.get_cache_stats();
                            let cache_memory_mb = status_blockchain.get_cache_memory_usage() / (1024 * 1024);
                            tracing::info!(
                                "💾 Block Cache: {} | Memory: {}MB",
                                cache_stats,
                                cache_memory_mb
                            );
                        }
                    }
                }
            }
        }
    });
    shutdown_manager.register_task(status_handle);

    // Spawn consensus cleanup task to prevent memory leaks
    // Cleans up finalized transactions older than 1 hour
    let cleanup_consensus = consensus_engine.clone();
    let cleanup_utxo = utxo_mgr.clone();
    let cleanup_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(600)); // Every 10 minutes
        loop {
            interval.tick().await;

            // Clean up consensus finalized transactions
            let removed = cleanup_consensus.cleanup_old_finalized(3600); // Keep 1 hour
            if removed > 0 {
                let stats = cleanup_consensus.memory_stats();
                tracing::info!(
                    "🧹 Consensus cleanup: removed {} old finalized txs. Current: {} tx_state, {} active_rounds, {} finalized",
                    removed,
                    stats.tx_state_entries,
                    stats.active_rounds,
                    stats.finalized_txs
                );
            }

            // Clean up transaction pool rejected transactions (older than 1 hour)
            cleanup_consensus.tx_pool.cleanup_rejected(3600);

            // Clean up expired UTXO locks (older than 10 minutes)
            let cleaned_locks = cleanup_utxo.cleanup_expired_locks();
            if cleaned_locks > 0 {
                tracing::info!("🧹 Cleaned {} expired UTXO locks", cleaned_locks);
            }

            tracing::debug!("🧹 Memory cleanup completed");
        }
    });
    shutdown_manager.register_task(cleanup_handle);

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

            // Start gossip-based masternode status tracking
            registry.start_gossip_broadcaster(peer_connection_registry.clone());
            registry.start_report_cleanup();
            tracing::info!("✓ Gossip-based masternode status tracking started");

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

            // CRITICAL: Wire up consensus broadcast callback for TimeVote requests
            // This enables the consensus engine to broadcast vote requests to the network
            let broadcast_registry = peer_connection_registry.clone();
            consensus_engine
                .set_broadcast_callback(move |msg| {
                    let registry = broadcast_registry.clone();
                    tokio::spawn(async move {
                        registry.broadcast(msg).await;
                    });
                })
                .await;
            tracing::info!("✓ Consensus broadcast callback configured");

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
            let rpc_blacklist = server.blacklist.clone();

            let rpc_handle = tokio::spawn(async move {
                match RpcServer::new(
                    &rpc_addr_clone,
                    rpc_consensus,
                    rpc_utxo,
                    rpc_network,
                    rpc_registry,
                    rpc_blockchain,
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
            println!("║  Consensus:  TSDC + timevote Hybrid                  ║");
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
