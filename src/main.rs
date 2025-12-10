mod address;
mod block;
mod config;
mod consensus;
mod network;
mod network_type;
mod rpc;
mod storage;
mod time_sync;
mod types;
mod utxo_manager;
mod vdf;
mod wallet;

use clap::Parser;
use config::Config;
use consensus::ConsensusEngine;
use network::server::NetworkServer;
use network_type::NetworkType;
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

    // Initialize masternode list
    let mut masternodes = Vec::new();

    // If masternode mode is enabled in config, register this node
    if config.masternode.enabled {
        let wallet_address = if config.masternode.wallet_address.is_empty() {
            // Use wallet address if not specified in config
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

        let collateral = match tier {
            types::MasternodeTier::Free => 0,
            types::MasternodeTier::Bronze => 1_000,
            types::MasternodeTier::Silver => 10_000,
            types::MasternodeTier::Gold => 100_000,
        };

        let masternode = types::Masternode {
            address: config.network.listen_address.clone(),
            wallet_address,
            collateral,
            tier: tier.clone(),
            public_key: *wallet.public_key(),
        };

        masternodes.push(masternode);
        println!("✓ Running as {:?} masternode", tier);
        println!("  └─ Wallet: {}", config.masternode.wallet_address);
        println!("  └─ Collateral: {} TIME", collateral);
    } else {
        println!("⚠ No masternodes registered - node will run in observer mode");
        println!("  To enable: Set masternode.enabled = true in config.toml");
    }

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

    let utxo_mgr = Arc::new(UTXOStateManager::new_with_storage(storage));

    let initial_utxo = UTXO {
        outpoint: OutPoint {
            txid: [0u8; 32],
            vout: 0,
        },
        value: 5000,
        script_pubkey: vec![],
        address: "sender".to_string(),
    };
    utxo_mgr.add_utxo(initial_utxo).await;
    println!("✓ Created initial UTXO (5000 TIME)\n");

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

    println!("✓ Ready to process transactions\n");

    // Start background NTP time synchronization
    time_sync.start_sync_task();

    let consensus = Arc::new(ConsensusEngine::new(masternodes, utxo_mgr.clone()));

    // Demo transaction (optional)
    if args.demo {
        println!("\n📡 Running demo transaction...");

        let tx = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: [0u8; 32],
                    vout: 0,
                },
                script_sig: vec![],
                sequence: 0xFFFFFFFF,
            }],
            outputs: vec![
                TxOutput {
                    value: 4000,
                    script_pubkey: vec![],
                },
                TxOutput {
                    value: 999,
                    script_pubkey: vec![],
                },
            ],
            lock_time: 0,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };

        match consensus.process_transaction(tx.clone()).await {
            Ok(_) => {
                println!("  ✅ Transaction finalized with BFT consensus!");
                println!("  └─ TXID: {}", ::hex::encode(tx.txid()));
            }
            Err(e) => {
                println!("  ❌ Transaction failed: {}", e);
            }
        }

        println!("\n🧱 Generating deterministic block...");
        let block = consensus
            .generate_deterministic_block(
                1,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
            )
            .await;

        println!("  ✅ Block produced:");
        println!("     Height:       {}", block.header.height);
        println!(
            "     Hash:         {}...",
            &::hex::encode(block.hash())[..16]
        );
        println!("     Transactions: {}", block.transactions.len());
        println!("     MN Rewards:   {}\n", block.masternode_rewards.len());
    } else {
        println!("✓ Ready to process transactions\n");
    }

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

    // Start network server

    println!("🌐 Starting P2P network server...");

    // Start RPC server
    let rpc_consensus = consensus.clone();
    let rpc_utxo = utxo_mgr.clone();
    let rpc_addr_clone = rpc_addr.clone();
    let rpc_network = network_type;

    tokio::spawn(async move {
        match RpcServer::new(&rpc_addr_clone, rpc_consensus, rpc_utxo, rpc_network).await {
            Ok(mut server) => {
                let _ = server.run().await;
            }
            Err(e) => {
                eprintln!("  ❌ Failed to start RPC server: {}", e);
            }
        }
    });

    match NetworkServer::new(&p2p_addr, utxo_mgr.clone(), consensus.clone()).await {
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
