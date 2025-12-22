mod address;
mod bft_consensus;
mod block;
mod blockchain;
mod config;
mod consensus;
mod heartbeat_attestation;
mod masternode_registry;
mod network;
mod network_type;
mod peer_manager;
mod rpc;
mod state_notifier;
mod storage;
mod time_sync;
mod transaction_pool;
mod types;
mod utxo_manager;
mod vdf;
mod wallet;

use bft_consensus::BFTConsensus;
use blockchain::Blockchain;
use chrono::Timelike;
use clap::Parser;
use config::Config;
use consensus::ConsensusEngine;
use heartbeat_attestation::HeartbeatAttestationSystem;
use masternode_registry::MasternodeRegistry;
use network::message::NetworkMessage;
use network::peer_connection_registry::PeerConnectionRegistry;
use network::peer_state::PeerStateManager;
use network::server::NetworkServer;
use network_type::NetworkType;
use peer_manager::PeerManager;
use rpc::server::RpcServer;
use std::sync::Arc;
use storage::{InMemoryUtxoStorage, UtxoStorage};
use time_sync::TimeSync;
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

    // Helper function to calculate appropriate cache size based on available memory
    fn calculate_cache_size() -> u64 {
        use sysinfo::System;
        let mut sys = System::new_all();
        sys.refresh_memory();
        let available_memory = sys.available_memory();

        // Use 10% of available memory per database, cap at 256MB each
        let cache_size = std::cmp::min(available_memory / 10, 256 * 1024 * 1024);

        tracing::info!(
            "📊 Configuring sled cache: {} MB (available memory: {} MB)",
            cache_size / (1024 * 1024),
            available_memory / (1024 * 1024)
        );

        cache_size
    }

    let cache_size = calculate_cache_size();

    // Initialize block storage
    let block_storage_path = format!("{}/blocks", config.storage.data_dir);
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
            .path(format!("{}/peers", config.storage.data_dir))
            .cache_capacity(cache_size)
            .open()
            .map_err(|e| format!("Failed to open peer database: {}", e))
            .unwrap(),
    );
    let peer_manager = Arc::new(PeerManager::new(peer_db, config.network.clone()));

    // Initialize masternode registry
    let registry_db_path = format!("{}/registry", config.storage.data_dir);
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

    // Initialize consensus engine
    let consensus_engine = Arc::new(ConsensusEngine::new(vec![], utxo_mgr.clone()));

    // Set identity if we're a masternode
    if let Some(ref mn) = masternode_info {
        consensus_engine
            .set_identity(mn.address.clone(), wallet.signing_key().clone())
            .await;
        tracing::info!(
            "✓ Consensus engine identity set for masternode: {}",
            mn.address
        );
    }

    // Set up broadcast callback for consensus engine
    let consensus_registry = registry.clone();
    consensus_engine
        .set_broadcast_callback(move |msg| {
            let registry = consensus_registry.clone();
            tokio::spawn(async move {
                registry.broadcast_message(msg).await;
            });
        })
        .await;

    // Start background task to sync masternodes from registry to consensus engine
    let consensus_sync = consensus_engine.clone();
    let registry_sync = registry.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;

            // Get current masternodes from registry
            let masternode_infos = registry_sync.get_all().await;
            let masternodes: Vec<Masternode> = masternode_infos
                .into_iter()
                .map(|info| info.masternode)
                .collect();

            // Update consensus engine with latest masternode list
            consensus_sync.update_masternodes(masternodes.clone()).await;
            tracing::debug!(
                "✅ Updated consensus engine with {} masternodes",
                masternodes.len()
            );
        }
    });

    // Initialize blockchain
    let blockchain = Arc::new(Blockchain::new(
        block_storage,
        consensus_engine.clone(),
        registry.clone(),
        network_type,
    ));

    // Set peer manager for fork consensus verification
    blockchain.set_peer_manager(peer_manager.clone()).await;

    // Initialize BFT consensus if running as masternode
    let bft_consensus = if let Some(ref mn) = masternode_info {
        let bft = Arc::new(BFTConsensus::new(mn.address.clone()));

        // Set up BFT to broadcast messages through registry
        let bft_registry = registry.clone();
        bft.set_broadcast_callback(move |msg| {
            let registry = bft_registry.clone();
            tokio::spawn(async move {
                // Broadcast through peer manager
                registry.broadcast_message(msg).await;
            });
        })
        .await;

        // Link BFT to blockchain for validation
        bft.set_blockchain(blockchain.clone()).await;

        // Link blockchain to BFT
        blockchain.set_bft_consensus(bft.clone()).await;

        tracing::info!("✓ BFT consensus initialized for masternode");
        Some(bft)
    } else {
        None
    };

    println!("✓ Blockchain initialized");
    println!();

    // Create shared connection manager for both client and server
    let connection_manager = Arc::new(network::connection_manager::ConnectionManager::new());

    // Create shared peer connection registry for managing active connections
    let peer_registry = Arc::new(PeerConnectionRegistry::new());

    // Create unified peer state manager for connection tracking
    let peer_state = Arc::new(PeerStateManager::new());

    // Set peer registry on blockchain for request/response queries
    blockchain.set_peer_registry(peer_registry.clone()).await;

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
        // Set local IP in connection manager for deterministic direction
        connection_manager.set_local_ip(ip.clone()).await;
    }

    // Start network client for outbound connections and masternode announcements
    let network_client = network::client::NetworkClient::new(
        peer_manager.clone(),
        registry.clone(),
        blockchain.clone(),
        attestation_system.clone(),
        network_type,
        config.network.max_peers as usize,
        connection_manager.clone(),
        peer_registry.clone(),
        peer_state.clone(),
        local_ip.clone(),
    );
    network_client.start().await;

    // Register this node if running as masternode
    if let Some(ref mn) = masternode_info {
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

                // Set signing key for BFT consensus
                if let Some(ref bft) = bft_consensus {
                    bft.set_signing_key(signing_key.clone()).await;
                    tracing::info!("✓ BFT consensus signing key configured");
                }

                tracing::info!("✓ Registered masternode: {}", mn.wallet_address);
                tracing::info!("✓ Heartbeat attestation identity configured");

                // Broadcast masternode announcement to the network so peers discover us
                let announcement = NetworkMessage::MasternodeAnnouncement {
                    address: mn.address.clone(),
                    reward_address: mn.wallet_address.clone(),
                    tier: mn.tier.clone(),
                    public_key: mn.public_key,
                };
                peer_registry.broadcast(announcement).await;
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
        let mn_clone = mn.clone();
        let peer_registry_clone = peer_registry.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;

                // Update old-style heartbeat
                if let Err(e) = registry_clone.heartbeat(&mn_address).await {
                    tracing::warn!("❌ Failed to send heartbeat: {}", e);
                }

                // Broadcast masternode announcement periodically so peers discover us
                let announcement = NetworkMessage::MasternodeAnnouncement {
                    address: mn_clone.address.clone(),
                    reward_address: mn_clone.wallet_address.clone(),
                    tier: mn_clone.tier.clone(),
                    public_key: mn_clone.public_key,
                };
                peer_registry_clone.broadcast(announcement).await;

                // Request masternodes from all connected peers for peer exchange
                tracing::info!("📤 Broadcasting GetMasternodes to all peers");
                peer_registry_clone
                    .broadcast(NetworkMessage::GetMasternodes)
                    .await;

                // Create and broadcast attestable heartbeat
                match attestation_clone.create_heartbeat().await {
                    Ok(heartbeat) => {
                        tracing::debug!(
                            "💓 Created signed heartbeat seq {}",
                            heartbeat.sequence_number
                        );
                        // Broadcast to network
                        registry_clone.broadcast_heartbeat(heartbeat).await;
                    }
                    Err(e) => {
                        tracing::warn!("❌ Failed to create attestable heartbeat: {}", e);
                    }
                }
            }
        });
    }

    // Initialize genesis and catchup in background
    let blockchain_init = blockchain.clone();
    let blockchain_server = blockchain_init.clone();
    tokio::spawn(async move {
        if let Err(e) = blockchain_init.initialize_genesis().await {
            tracing::error!("❌ Genesis initialization failed: {}", e);
            return;
        }

        if let Err(e) = blockchain_init.catchup_blocks().await {
            tracing::error!("❌ Block catchup failed: {}", e);
        }

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
            let expected_height = block_blockchain.calculate_expected_height();

            // Determine what to do based on height comparison
            if current_height < expected_height - 1 {
                // More than 1 block behind - need catchup
                tracing::info!(
                    "🧱 Catching up: height {} → {} at {} ({}:{}0) with {} eligible masternodes",
                    current_height,
                    expected_height,
                    timestamp,
                    now.hour(),
                    (now.minute() / 10),
                    masternodes.len()
                );

                match block_blockchain.catchup_blocks().await {
                    Ok(()) => {
                        tracing::info!("✅ Catchup complete");
                    }
                    Err(e) => {
                        tracing::error!("❌ Failed to catchup blocks: {}", e);
                        continue;
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
                let is_producer = masternode_info
                    .as_ref()
                    .map(|mn| mn.address == selected_producer.address)
                    .unwrap_or(false);

                if is_producer {
                    tracing::info!(
                        "🎯 Selected as block producer for height {} at {} ({}:{}0)",
                        current_height + 1,
                        timestamp,
                        now.hour(),
                        (now.minute() / 10),
                    );

                    match block_blockchain.produce_block().await {
                        Ok(block) => {
                            tracing::info!(
                                "✅ Block {} produced: {} transactions, {} masternode rewards",
                                block.header.height,
                                block.transactions.len(),
                                block.masternode_rewards.len()
                            );

                            // Broadcast block to all peers
                            block_registry.broadcast_block(block).await;
                        }
                        Err(e) => {
                            tracing::error!("❌ Failed to produce block: {}", e);
                        }
                    }
                } else {
                    tracing::debug!(
                        "⏸️  Not selected for block {} (producer: {})",
                        current_height + 1,
                        selected_producer.address
                    );
                }
            } else {
                tracing::warn!(
                    "⚠️ Height {} ahead of expected {}, skipping block production",
                    current_height,
                    expected_height
                );
            }
        }
    });

    // Start BFT committed block processor (every 5 seconds)
    if bft_consensus.is_some() {
        let bft_blockchain = blockchain.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            loop {
                interval.tick().await;

                // Process any BFT-committed blocks
                match bft_blockchain.process_bft_committed_blocks().await {
                    Ok(count) if count > 0 => {
                        tracing::info!("✅ Processed {} BFT-committed block(s)", count);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!("❌ Failed to process BFT-committed blocks: {}", e);
                    }
                }
            }
        });
    }

    // Start network server

    println!("🌐 Starting P2P network server...");

    // Start RPC server
    let rpc_consensus = consensus_engine.clone();
    let rpc_utxo = utxo_mgr.clone();
    let rpc_registry = registry.clone();
    let rpc_blockchain = blockchain.clone();
    let rpc_addr_clone = rpc_addr.clone();
    let rpc_network = network_type;

    tokio::spawn(async move {
        match RpcServer::new(
            &rpc_addr_clone,
            rpc_consensus,
            rpc_utxo,
            rpc_network,
            rpc_registry,
            rpc_blockchain,
            attestation_system.clone(),
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

    // Periodic status report - logs at :05, :15, :25, :35, :45, :55 (midway between block times)
    let status_blockchain = blockchain_server.clone();
    let status_registry = registry.clone();
    tokio::spawn(async move {
        loop {
            // Wait until next 5-minute mark (:05, :15, :25, :35, :45, :55)
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let minute = (now / 60) % 60;
            let second = now % 60;

            // Calculate seconds until next 5-minute mark (at :05, :15, :25, :35, :45, :55)
            let target_minute = ((minute / 10) * 10) + 5;
            let next_target = if minute < target_minute {
                target_minute
            } else {
                ((minute / 10) * 10) + 15
            };

            let minutes_until = if next_target > minute {
                next_target - minute
            } else {
                60 - minute + next_target
            };

            let seconds_until = (minutes_until * 60) - second;

            tokio::time::sleep(tokio::time::Duration::from_secs(seconds_until)).await;

            let height = status_blockchain.get_height().await;
            let mn_count = status_registry.list_active().await.len();
            tracing::info!(
                "📊 Status: Height={}, Active Masternodes={}",
                height,
                mn_count
            );
        }
    });

    match NetworkServer::new(
        &p2p_addr,
        utxo_mgr.clone(),
        consensus_engine.clone(),
        registry.clone(),
        blockchain_server.clone(),
        peer_manager.clone(),
        connection_manager.clone(),
        peer_registry.clone(),
        peer_state.clone(),
        local_ip.clone(),
    )
    .await
    {
        Ok(mut server) => {
            // Give registry access to network broadcast channel
            registry
                .set_broadcast_channel(server.tx_notifier.clone())
                .await;

            println!("  ✅ Network server listening on {}", p2p_addr);
            println!("\n╔═══════════════════════════════════════════════════════╗");
            println!("║  🎉 TIME Coin Daemon is Running!                      ║");
            println!("╠═══════════════════════════════════════════════════════╣");
            println!("║  Network:    {:<40} ║", format!("{:?}", network_type));
            println!("║  Storage:    {:<40} ║", config.storage.backend);
            println!("║  P2P Port:   {:<40} ║", p2p_addr);
            println!("║  RPC Port:   {:<40} ║", rpc_addr);
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
