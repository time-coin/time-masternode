mod address;
mod block;
mod blockchain;
mod config;
mod consensus;
mod masternode_registry;
mod network;
mod network_type;
mod peer_manager;
mod rpc;
mod storage;
mod time_sync;
mod transaction_pool;
mod types;
mod utxo_manager;
mod vdf;
mod wallet;

use blockchain::Blockchain;
use chrono::Timelike;
use clap::Parser;
use config::Config;
use consensus::ConsensusEngine;
use masternode_registry::MasternodeRegistry;
use network::server::NetworkServer;
use network_type::NetworkType;
use peer_manager::PeerManager;
use rpc::server::RpcServer;
use std::sync::Arc;
use storage::{InMemoryUtxoStorage, UtxoStorage};
use time_sync::TimeSync;
use types::*;
use utxo_manager::UTXOStateManager;
use vdf::VDFConfig;
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

    let network_type = config.node.network_type();
    let p2p_addr = config.network.full_listen_address(&network_type);
    let rpc_addr = config.rpc.full_listen_address(&network_type);

    println!("\n🚀 TIME Coin Protocol Daemon v{}", config.node.version);
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
        let wallet_address = if config.masternode.wallet_address.is_empty() {
            wallet.address().to_string()
        } else {
            config.masternode.wallet_address.clone()
        };

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

        let masternode = types::Masternode {
            address: config.network.full_listen_address(&network_type),
            wallet_address: wallet_address.clone(),
            collateral: tier.collateral(),
            tier: tier.clone(),
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
            println!("  └─ Data directory: {}", config.storage.data_dir);
            match storage::SledUtxoStorage::new(&config.storage.data_dir) {
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

    // Initialize block storage
    let block_storage_path = format!("{}/blocks", config.storage.data_dir);
    let block_storage = match sled::open(&block_storage_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ Failed to initialize block storage: {}", e);
            std::process::exit(1);
        }
    };

    let utxo_mgr = Arc::new(UTXOStateManager::new_with_storage(storage));

    // Initialize peer manager
    println!("🔍 Initializing peer manager...");
    let peer_db = Arc::new(
        sled::open(format!("{}/peers", config.storage.data_dir))
            .map_err(|e| format!("Failed to open peer database: {}", e))
            .unwrap(),
    );
    let peer_manager = Arc::new(PeerManager::new(peer_db, config.network.clone()));
    if let Err(e) = peer_manager.initialize().await {
        eprintln!("⚠️ Peer manager initialization warning: {}", e);
    }
    println!("  ✅ Peer manager initialized");
    println!();

    // Initialize masternode registry
    let registry_db_path = format!("{}/registry", config.storage.data_dir);
    let registry_db = Arc::new(match sled::open(&registry_db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("❌ Failed to open registry database: {}", e);
            std::process::exit(1);
        }
    });
    let registry = Arc::new(MasternodeRegistry::new(registry_db.clone(), network_type));
    registry.set_peer_manager(peer_manager.clone()).await;

    // Register this node if running as masternode
    if let Some(ref mn) = masternode_info {
        match registry
            .register(mn.clone(), mn.wallet_address.clone())
            .await
        {
            Ok(()) => {
                tracing::info!("✓ Registered masternode: {}", mn.wallet_address);
            }
            Err(masternode_registry::RegistryError::AlreadyRegistered) => {
                tracing::info!("✓ Masternode already registered: {}", mn.wallet_address);
            }
            Err(e) => {
                tracing::error!("❌ Failed to register masternode: {}", e);
                std::process::exit(1);
            }
        }

        // Start heartbeat task for this masternode
        let registry_clone = registry.clone();
        let mn_address = mn.address.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Err(e) = registry_clone.heartbeat(&mn_address).await {
                    tracing::warn!("❌ Failed to send heartbeat: {}", e);
                }
            }
        });
    }

    println!("✓ Ready to process transactions\n");

    // Initialize consensus engine
    let consensus_engine = Arc::new(ConsensusEngine::new(vec![], utxo_mgr.clone()));

    // Initialize blockchain
    let vdf_config = match network_type {
        NetworkType::Mainnet => VDFConfig::mainnet(),
        NetworkType::Testnet => VDFConfig::testnet(),
    };

    let blockchain = Arc::new(Blockchain::new(
        block_storage,
        consensus_engine.clone(),
        registry.clone(),
        vdf_config,
    ));

    println!("✓ Blockchain initialized");
    println!();

    // Initialize genesis and catchup in background
    let blockchain_init = blockchain.clone();
    tokio::spawn(async move {
        if let Err(e) = blockchain_init.initialize_genesis().await {
            tracing::error!("❌ Genesis initialization failed: {}", e);
            return;
        }

        if let Err(e) = blockchain_init.catchup_blocks().await {
            tracing::error!("❌ Block catchup failed: {}", e);
            return;
        }

        if let Err(e) = blockchain_init.start_block_production().await {
            tracing::error!("❌ Block production failed: {}", e);
        }
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
        println!("🔍 Discovering peers from time-coin.io...");
        let discovery = network::peer_discovery::PeerDiscovery::new(
            "https://time-coin.io/api/peers".to_string(),
        );

        let fallback_peers = config.network.bootstrap_peers.clone();
        let discovered_peers = discovery.fetch_peers_with_fallback(fallback_peers).await;

        println!("  ✅ Loaded {} peer(s)", discovered_peers.len());
        for peer in discovered_peers.iter().take(3) {
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
    tokio::spawn(async move {
        // Calculate time until next 10-minute boundary
        let now = chrono::Utc::now();
        let minute = now.minute();
        let seconds_into_period = (minute % 10) * 60 + now.second();
        let seconds_until_next = 600 - seconds_into_period;

        // Wait until the next 10-minute boundary
        tokio::time::sleep(tokio::time::Duration::from_secs(seconds_until_next as u64)).await;

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(600)); // 10 minutes
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

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
            let masternodes: Vec<Masternode> = eligible.iter().map(|(mn, _)| mn.clone()).collect();

            // Require at least 3 masternodes for block production
            if masternodes.len() < 3 {
                tracing::warn!(
                    "⚠️ Skipping block production: only {} masternodes active (minimum 3 required)",
                    masternodes.len()
                );
                continue;
            }

            let current_height = block_blockchain.get_height().await;
            let expected_height =
                block::generator::DeterministicBlockGenerator::calculate_expected_height();

            tracing::info!(
                "🧱 Producing block at height {} (expected: {}) at {} ({}:{}0) with {} eligible masternodes",
                current_height + 1,
                expected_height,
                timestamp,
                now.hour(),
                (now.minute() / 10),
                masternodes.len()
            );

            // Generate catchup blocks if needed
            if current_height < expected_height {
                tracing::warn!(
                    "⏩ Chain is behind! Generating {} catchup blocks...",
                    expected_height - current_height
                );
                match block_blockchain.catchup_blocks().await {
                    Ok(()) => {
                        tracing::info!("✅ Catchup complete");
                    }
                    Err(e) => {
                        tracing::error!("❌ Failed to catchup blocks: {}", e);
                    }
                }
            } else {
                // Produce next block (includes finalized transactions and fees)
                match block_blockchain.produce_block().await {
                    Ok(block) => {
                        tracing::info!(
                            "✅ Block {} produced: {} transactions, {} masternode rewards",
                            block.header.height,
                            block.transactions.len(),
                            block.masternode_rewards.len()
                        );
                    }
                    Err(e) => {
                        tracing::error!("❌ Failed to produce block: {}", e);
                    }
                }
            }
        }
    });

    // Start network server

    println!("🌐 Starting P2P network server...");

    // Start RPC server
    let rpc_consensus = consensus_engine.clone();
    let rpc_utxo = utxo_mgr.clone();
    let rpc_registry = registry.clone();
    let rpc_addr_clone = rpc_addr.clone();
    let rpc_network = network_type;

    tokio::spawn(async move {
        match RpcServer::new(
            &rpc_addr_clone,
            rpc_consensus,
            rpc_utxo,
            rpc_network,
            rpc_registry,
        )
        .await
        {
            Ok(mut server) => {
                let _ = server.run().await;
            }
            Err(e) => {
                eprintln!("  ❌ Failed to start RPC server: {}", e);
            }
        }
    });

    match NetworkServer::new(&p2p_addr, utxo_mgr.clone(), consensus_engine.clone()).await {
        Ok(mut server) => {
            println!("  ✅ Network server listening on {}", p2p_addr);
            println!("\n╔═══════════════════════════════════════════════════════╗");
            println!("║  🎉 TIME Coin Daemon is Running!                      ║");
            println!("╠═══════════════════════════════════════════════════════╣");
            println!("║  Network:    {:<41} ║", format!("{:?}", network_type));
            println!("║  Storage:    {:<41} ║", config.storage.backend);
            println!("║  P2P Port:   {:<41} ║", p2p_addr);
            println!("║  RPC Port:   {:<41} ║", rpc_addr);
            println!("║  Consensus:  BFT (2/3 quorum)                         ║");
            println!("║  Finality:   Instant (<3 seconds)                     ║");
            println!("╚═══════════════════════════════════════════════════════╝");
            println!("\nPress Ctrl+C to stop\n");

            if let Err(e) = server.run().await {
                println!("❌ Server error: {}", e);
            }
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

    match config.format.as_str() {
        "json" => {
            fmt().json().with_env_filter(filter).init();
        }
        _ => {
            // Pretty format with less clutter
            fmt()
                .with_env_filter(filter)
                .with_target(false) // Hide module targets
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_file(false) // Hide file locations
                .with_line_number(false)
                .compact() // Use compact format
                .init();
        }
    }
}
