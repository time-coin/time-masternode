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
pub mod masternode_certificate;
pub mod masternode_registry;
pub mod network;
pub mod network_type;
pub mod peer_manager;
pub mod rpc;
pub mod shutdown;
pub mod state_notifier;
pub mod storage;
pub mod time_sync;
pub mod timelock;
pub mod timevote;
pub mod transaction_pool;
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
use types::*;
use utxo_manager::UTXOStateManager;
use wallet::WalletManager;

#[derive(Parser, Debug)]
#[command(name = "timed")]
#[command(about = "TIME Coin Protocol Daemon", long_about = None)]
struct Args {
    /// Config file path (time.conf or legacy TOML)
    #[arg(short, long, alias = "config")]
    conf: Option<String>,

    /// Data directory override
    #[arg(long)]
    datadir: Option<String>,

    /// Run on testnet (overrides config file)
    #[arg(long)]
    testnet: bool,

    #[arg(long)]
    listen_addr: Option<String>,

    #[arg(long)]
    masternode: bool,

    #[arg(short, long)]
    verbose: bool,

    /// Run demo transaction on startup
    #[arg(long)]
    demo: bool,

    /// Generate default time.conf and masternode.conf, then exit
    #[arg(long)]
    generate_config: bool,
}

#[tokio::main]
async fn main() {
    // Install rustls crypto provider before any TLS usage
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls CryptoProvider");

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

    // ─── Determine config path and network type ──────────────────────
    // Priority: --conf flag > time.conf in data dirs > legacy TOML fallback
    let conf_path = if let Some(ref p) = args.conf {
        std::path::PathBuf::from(p)
    } else {
        let base_dir = config::get_data_dir();
        let testnet_dir = config::get_network_data_dir(&NetworkType::Testnet);

        if args.testnet {
            // Explicit --testnet: check testnet dir first
            if testnet_dir.join("time.conf").exists() {
                testnet_dir.join("time.conf")
            } else if testnet_dir.join("config.toml").exists() {
                testnet_dir.join("config.toml")
            } else {
                testnet_dir.join("time.conf")
            }
        } else if base_dir.join("time.conf").exists() {
            base_dir.join("time.conf")
        } else if base_dir.join("config.toml").exists() {
            base_dir.join("config.toml")
        } else if testnet_dir.join("time.conf").exists() {
            testnet_dir.join("time.conf")
        } else if testnet_dir.join("config.toml").exists() {
            // Legacy: TOML in testnet dir (common existing setup)
            testnet_dir.join("config.toml")
        } else if std::path::Path::new("config.toml").exists() {
            // Legacy fallback — TOML in CWD
            std::path::PathBuf::from("config.toml")
        } else {
            // No config found anywhere — default to mainnet base dir
            base_dir.join("time.conf")
        }
    };

    let is_legacy_toml = conf_path.extension().is_some_and(|ext| ext == "toml");

    // Determine network type
    let in_testnet_dir = conf_path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|name| name == "testnet");

    let network_type = if args.testnet {
        NetworkType::Testnet
    } else if is_legacy_toml {
        if let Ok(cfg) = Config::load_from_file(&conf_path.to_string_lossy()) {
            cfg.node.network_type()
        } else if in_testnet_dir {
            NetworkType::Testnet
        } else {
            NetworkType::Mainnet
        }
    } else {
        config::detect_network_from_conf(&conf_path)
    };

    if args.generate_config {
        let data_dir = config::get_network_data_dir(&network_type);
        std::fs::create_dir_all(&data_dir).ok();
        let gen_conf = data_dir.join("time.conf");
        let gen_mn = data_dir.join("masternode.conf");
        match config::generate_default_conf_for_network(&gen_conf, &network_type) {
            Ok(_) => println!("✅ Generated {}", gen_conf.display()),
            Err(e) => {
                eprintln!("❌ Failed to generate time.conf: {}", e);
                std::process::exit(1);
            }
        }
        match config::generate_default_masternode_conf(&gen_mn) {
            Ok(_) => println!("✅ Generated {}", gen_mn.display()),
            Err(e) => {
                eprintln!("❌ Failed to generate masternode.conf: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    // Load config — time.conf or legacy TOML
    // If a legacy TOML path was specified but doesn't exist, use time.conf instead
    let (conf_path, is_legacy_toml) = if is_legacy_toml && !conf_path.exists() {
        let new_path = conf_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("time.conf");
        println!(
            "  ℹ️ {} not found, using {} instead",
            conf_path.display(),
            new_path.display()
        );
        (new_path, false)
    } else {
        (conf_path, is_legacy_toml)
    };

    let mut config = if is_legacy_toml {
        match Config::load_or_create(&conf_path.to_string_lossy(), &network_type) {
            Ok(cfg) => {
                println!(
                    "  ✓ Loaded legacy configuration from {}",
                    conf_path.display()
                );
                // Generate time.conf + masternode.conf alongside legacy TOML
                // so the user has them ready for migration
                let conf_dir = conf_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let new_conf = conf_dir.join("time.conf");
                let new_mn = conf_dir.join("masternode.conf");
                if !new_conf.exists() {
                    if let Err(e) =
                        config::generate_default_conf_for_network(&new_conf, &network_type)
                    {
                        eprintln!("  ⚠️ Could not generate time.conf: {}", e);
                    } else {
                        println!(
                            "  ✓ Generated {} (migrate from legacy TOML when ready)",
                            new_conf.display()
                        );
                    }
                }
                if !new_mn.exists() {
                    if let Err(e) = config::generate_default_masternode_conf(&new_mn) {
                        eprintln!("  ⚠️ Could not generate masternode.conf: {}", e);
                    } else {
                        println!("  ✓ Generated {}", new_mn.display());
                    }
                }
                // Even in legacy TOML mode, load masternode.conf for collateral
                // and time.conf for masternodeprivkey (the new config files)
                let mut cfg = cfg;
                if new_mn.exists() {
                    match config::parse_masternode_conf(&new_mn) {
                        Ok(entries) => {
                            if let Some(entry) = entries.first() {
                                cfg.masternode.collateral_txid = entry.collateral_txid.clone();
                                cfg.masternode.collateral_vout = entry.collateral_vout;
                                if !entry.address.is_empty() {
                                    cfg.network.external_address = Some(entry.address.clone());
                                }
                                println!("  ✓ Loaded masternode.conf: alias={}", entry.alias);
                            }
                        }
                        Err(e) => eprintln!("  ⚠️ Could not parse masternode.conf: {}", e),
                    }
                }
                if new_conf.exists() {
                    if let Ok(conf_values) = config::parse_conf_file(&new_conf) {
                        if let Some(keys) = conf_values.get("masternodeprivkey") {
                            if let Some(key) = keys.first() {
                                cfg.masternode.masternodeprivkey = key.clone();
                                println!("  ✓ Loaded masternodeprivkey from time.conf");
                            }
                        }
                    }
                }
                cfg
            }
            Err(e) => {
                eprintln!("❌ Failed to load config: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match Config::load_from_conf(&conf_path, &network_type) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("❌ Failed to load config: {}", e);
                std::process::exit(1);
            }
        }
    };

    // CLI overrides
    if args.testnet {
        config.node.network = "testnet".to_string();
    }
    if let Some(ref datadir) = args.datadir {
        config.storage.data_dir = datadir.clone();
    }

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
            println!("  └─ File: {}", wallet_manager.default_wallet_path());
            w
        }
        Err(e) => {
            eprintln!("❌ Failed to initialize wallet: {}", e);
            std::process::exit(1);
        }
    };
    println!();

    // Decode masternodeprivkey from time.conf if provided (used as consensus signing key)
    let masternode_signing_key: Option<ed25519_dalek::SigningKey> =
        if !config.masternode.masternodeprivkey.is_empty() {
            match masternode_certificate::decode_masternode_key(
                &config.masternode.masternodeprivkey,
            ) {
                Ok(secret_bytes) => {
                    let key = ed25519_dalek::SigningKey::from_bytes(&secret_bytes);
                    println!("✓ Loaded masternodeprivkey from time.conf");
                    Some(key)
                }
                Err(e) => {
                    eprintln!(
                        "⚠️ Invalid masternodeprivkey in time.conf: {} — using wallet key",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

    // Public key for masternode identity: from masternodeprivkey if set, else wallet
    let mn_public_key = masternode_signing_key
        .as_ref()
        .map(|k| k.verifying_key())
        .unwrap_or(*wallet.public_key());

    // Initialize masternode info for later registration
    let mut masternode_info: Option<types::Masternode> = if config.masternode.enabled {
        // Always use the wallet's address (auto-generated per node)
        let wallet_address = wallet.address().to_string();

        // Get external address and extract IP only (no port) for consistent masternode identification
        let full_address = config.network.full_external_address(&network_type);
        let ip_only = full_address
            .split(':')
            .next()
            .unwrap_or(&full_address)
            .to_string();

        // Parse collateral outpoint if provided (for staked tiers)
        let has_collateral = !config.masternode.collateral_txid.is_empty();

        // Determine tier: auto-detect from collateral UTXO, or use explicit config
        let tier = match config.masternode.tier.to_lowercase().as_str() {
            "" | "auto" => {
                if has_collateral {
                    // Tier will be determined after UTXO lookup — use placeholder
                    // We'll resolve it below when we have the outpoint
                    None
                } else {
                    Some(types::MasternodeTier::Free)
                }
            }
            "free" => Some(types::MasternodeTier::Free),
            "bronze" => Some(types::MasternodeTier::Bronze),
            "silver" => Some(types::MasternodeTier::Silver),
            "gold" => Some(types::MasternodeTier::Gold),
            _ => {
                eprintln!(
                    "❌ Error: Invalid masternode tier '{}' (must be auto/free/bronze/silver/gold)",
                    config.masternode.tier
                );
                std::process::exit(1);
            }
        };

        let masternode = if has_collateral && tier != Some(types::MasternodeTier::Free) {
            let txid_bytes = hex::decode(&config.masternode.collateral_txid).unwrap_or_else(|_| {
                eprintln!(
                    "❌ Error: Invalid collateral_txid hex '{}'",
                    config.masternode.collateral_txid
                );
                std::process::exit(1);
            });
            if txid_bytes.len() != 32 {
                eprintln!("❌ Error: collateral_txid must be 32 bytes (64 hex chars)");
                std::process::exit(1);
            }
            let mut txid = [0u8; 32];
            txid.copy_from_slice(&txid_bytes);
            let outpoint = types::OutPoint {
                txid,
                vout: config.masternode.collateral_vout,
            };

            // If tier is auto (None), resolve from collateral UTXO value at startup.
            // The UTXO manager isn't available yet, so we look up the value from storage.
            // For now, use the explicit tier if set, or defer detection to registration.
            let resolved_tier = match tier {
                Some(t) => t,
                None => {
                    // Auto-detect: try to determine from on-chain UTXO after storage is ready.
                    // At this point we don't have the UTXO manager yet, so we store None
                    // and resolve after storage init. For now, log and defer.
                    println!(
                        "  ℹ️  Tier auto-detection enabled — will resolve from collateral UTXO"
                    );
                    types::MasternodeTier::Free // Placeholder, resolved below
                }
            };

            types::Masternode::new_with_collateral(
                ip_only,
                wallet_address.clone(),
                resolved_tier.collateral(),
                outpoint,
                mn_public_key,
                resolved_tier,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            )
        } else {
            let resolved_tier = tier.unwrap_or(types::MasternodeTier::Free);
            types::Masternode::new_legacy(
                ip_only,
                wallet_address.clone(),
                resolved_tier.collateral(),
                mn_public_key,
                resolved_tier,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            )
        };

        // Validate external address matches actual public IP
        let mut masternode = masternode;
        if let Ok(public_ip_output) = std::process::Command::new("curl")
            .args(["-s", "--max-time", "5", "https://api.ipify.org"])
            .output()
        {
            if public_ip_output.status.success() {
                if let Ok(detected_ip) = String::from_utf8(public_ip_output.stdout) {
                    let detected_ip = detected_ip.trim().to_string();
                    if !detected_ip.is_empty()
                        && detected_ip.parse::<std::net::IpAddr>().is_ok()
                        && detected_ip != masternode.address
                    {
                        eprintln!(
                            "⚠️  Config external IP ({}) does not match detected public IP ({})",
                            masternode.address, detected_ip
                        );
                        eprintln!("  └─ Using detected IP: {}", detected_ip);
                        masternode.address = detected_ip;
                    }
                }
            }
        }

        let display_tier = masternode.tier;
        let auto_detecting = has_collateral && display_tier == types::MasternodeTier::Free;
        if !auto_detecting {
            println!("✓ Running as {:?} masternode", display_tier);
            println!("  └─ Wallet: {}", wallet_address);
            println!(
                "  └─ Collateral: {} TIME",
                display_tier.collateral() / 100_000_000
            );
        }
        Some(masternode)
    } else {
        println!("⚠ No masternode configured - node will run in observer mode");
        println!("  To enable: Set masternode=1 in time.conf");
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

        // Check cgroup memory limit (common in containers/systemd services)
        let cgroup_limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .or_else(|_| std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());

        let effective_memory = match cgroup_limit {
            Some(limit) if limit < available_memory => limit,
            _ => available_memory,
        };

        // Use 10% of effective memory per database, cap at 256MB each
        let cache_size = std::cmp::min(effective_memory / 10, 256 * 1024 * 1024);

        tracing::info!(
            cache_mb = cache_size / (1024 * 1024),
            available_mb = effective_memory / (1024 * 1024),
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
        .flush_every_ms(None) // Disable auto-flush; we flush manually after each block write
        .mode(sled::Mode::LowSpace) // More conservative writes to prevent corruption
        .open()
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("❌ Failed to initialize block storage: {}", e);
            std::process::exit(1);
        }
    };

    let mut utxo_mgr = UTXOStateManager::new_with_storage(storage);

    // Enable persistent collateral lock storage
    if let Err(e) = utxo_mgr.enable_collateral_persistence(&block_storage) {
        tracing::warn!("⚠️ Failed to enable collateral persistence: {:?}", e);
    }

    let utxo_mgr = Arc::new(utxo_mgr);

    // Initialize UTXO states from storage
    tracing::info!("🔧 Initializing UTXO state manager from storage...");
    if let Err(e) = utxo_mgr.initialize_states().await {
        eprintln!("⚠️ Warning: Failed to initialize UTXO states: {}", e);
    }

    // Load persisted collateral locks from disk (must be after initialize_states
    // so UTXO states are available for validation)
    let loaded_locks = utxo_mgr.load_persisted_collateral_locks();
    if loaded_locks > 0 {
        tracing::info!(
            "✅ Restored {} collateral lock(s) from persistent storage",
            loaded_locks
        );
    }

    // Auto-detect masternode tier from collateral UTXO value
    if let Some(ref mut mn) = masternode_info {
        if let (types::MasternodeTier::Free, Some(outpoint)) =
            (mn.tier, mn.collateral_outpoint.as_ref())
        {
            // Tier was set to Free as placeholder for auto-detection
            match utxo_mgr.get_utxo(outpoint).await {
                Ok(utxo) => {
                    if let Some(detected_tier) =
                        types::MasternodeTier::from_collateral_value(utxo.value)
                    {
                        println!("✓ Running as {:?} masternode", detected_tier);
                        println!("  └─ Wallet: {}", mn.address);
                        println!(
                            "  └─ Collateral: {} TIME (auto-detected from UTXO)",
                            utxo.value / 100_000_000
                        );
                        mn.tier = detected_tier;
                        mn.collateral = detected_tier.collateral();
                    } else {
                        eprintln!(
                            "❌ Error: Collateral UTXO value {} TIME doesn't match any tier (need 1000/10000/100000 TIME)",
                            utxo.value / 100_000_000
                        );
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "⚠️ Warning: Could not look up collateral UTXO for tier auto-detection: {}",
                        e
                    );
                    eprintln!("   Node will start as Free tier. Set tier= in time.conf or ensure collateral UTXO exists.");
                    println!("✓ Running as Free masternode");
                    println!("  └─ Wallet: {}", mn.address);
                    println!("  └─ Collateral: 0 TIME");
                }
            }
        }
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

    // Keep a reference for flushing on shutdown
    let block_storage_for_shutdown = block_storage.clone();

    // Initialize AI System with all modules
    let ai_system = match ai::AISystem::new(Arc::new(block_storage.clone())) {
        Ok(system) => {
            tracing::info!("🧠 AI System initialized successfully");
            Arc::new(system)
        }
        Err(e) => {
            tracing::error!("❌ Failed to initialize AI System: {}", e);
            std::process::exit(1);
        }
    };

    // Enable AI validation using the same db as block storage
    consensus_engine.enable_ai_validation(Arc::new(block_storage.clone()));

    let consensus_engine = Arc::new(consensus_engine);
    tracing::info!("✓ Consensus engine initialized with AI validation and TimeLock voting");

    // Initialize blockchain
    let mut blockchain = Blockchain::new(
        block_storage,
        consensus_engine.clone(),
        registry.clone(),
        utxo_mgr.clone(),
        network_type,
    );

    // Configure block compression from config
    blockchain.set_compress_blocks(config.storage.compress_blocks);

    // Set AI system on blockchain for intelligent decision making
    blockchain.set_ai_system(ai_system.clone());

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

    // Verify chain height integrity on startup (fix inconsistencies from crashes)
    tracing::info!("🔍 Verifying chain height integrity...");
    match blockchain.verify_and_fix_chain_height() {
        Ok(true) => {
            tracing::info!("✅ Chain height was corrected during startup verification");
        }
        Ok(false) => {
            tracing::debug!("✓ Chain height is consistent");
        }
        Err(e) => {
            tracing::warn!("⚠️ Chain height verification failed: {}", e);
        }
    }

    // Validate existing blockchain on startup
    let current_height = blockchain.get_height();

    match blockchain.get_block_by_height(0).await {
        Ok(_genesis) => {
            // We have a genesis block
            tracing::info!(
                "✅ Genesis block exists (current height: {})",
                current_height
            );
        }
        Err(_) if current_height > 0 => {
            // Height > 0 but no genesis block - corrupted database
            eprintln!(
                "❌ CRITICAL: Genesis block not found but height is {}",
                current_height
            );
            eprintln!("   Blockchain database is corrupted");
            eprintln!("   Manual fix: Clear blockchain data");
            eprintln!("   Command: rm -rf {}/db/blocks", config.storage.data_dir);
            std::process::exit(1);
        }
        Err(_) => {
            // No genesis block and height is 0 - fresh start
            tracing::info!(
                "📋 No existing blockchain - will participate in dynamic genesis election"
            );
        }
    }

    // Migrate old-schema blocks before doing anything else
    tracing::info!("🔄 Running schema migration check...");
    match blockchain.migrate_old_schema_blocks().await {
        Ok(count) => {
            if count > 0 {
                tracing::info!("✅ Migrated {} old-schema blocks", count);
            }
        }
        Err(e) => {
            tracing::error!("❌ Schema migration failed: {}", e);
            tracing::error!(
                "   You may need to clear the database: rm -rf {}/.timecoin/{:?}/db/blocks",
                std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()),
                network_type
            );
        }
    }

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
            tracing::error!("❌ Manual intervention required: see analysis/CONSENSUS_FIX.md");
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
                // Repair corrupt blocks by re-fetching from peers
                match blockchain.repair_corrupt_blocks(&corrupt_blocks).await {
                    Ok(repaired) => {
                        tracing::info!(
                            "✅ Repaired {}/{} corrupt blocks from peers",
                            repaired,
                            corrupt_blocks.len()
                        );
                    }
                    Err(e) => {
                        tracing::error!("❌ Failed to repair corrupt blocks: {}", e);
                    }
                }
            } else {
                tracing::info!("✅ Chain integrity validation passed");
            }
        }
        Err(e) => {
            tracing::error!("❌ Chain integrity validation error: {}", e);
        }
    }

    // Check for missing blocks in the chain (continuity check)
    tracing::info!("🔍 Checking blockchain continuity...");
    let missing_blocks = blockchain.check_chain_continuity();
    if !missing_blocks.is_empty() {
        tracing::warn!(
            "⚠️ Detected {} missing blocks in chain",
            missing_blocks.len()
        );

        // Diagnose first 50 missing blocks in detail
        if !missing_blocks.is_empty() {
            let diagnose_end =
                std::cmp::min(missing_blocks[0] + 50, *missing_blocks.last().unwrap());
            blockchain.diagnose_missing_blocks(missing_blocks[0], diagnose_end);
        }

        // Note: Will request missing blocks from peers after network is initialized
    } else {
        tracing::info!("✅ Blockchain continuity verified");
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

                // Store empty certificate in registry (certificate system removed)
                registry.set_local_certificate([0u8; 64]).await;

                // Set signing key: use masternodeprivkey from time.conf if provided,
                // otherwise fall back to the wallet's auto-generated key
                let signing_key = if let Some(ref mn_key) = masternode_signing_key {
                    tracing::info!("✓ Using masternodeprivkey for consensus signing");
                    mn_key.clone()
                } else {
                    tracing::info!("✓ Using wallet key for consensus signing (no masternodeprivkey in time.conf)");
                    wallet.signing_key().clone()
                };
                if let Err(e) =
                    consensus_engine.set_identity(mn.address.clone(), signing_key.clone())
                {
                    eprintln!("⚠️ Failed to set consensus identity: {}", e);
                }

                tracing::info!("✓ Registered masternode: {}", mn.wallet_address);
                tracing::info!("✓ Consensus engine identity configured with wallet key");

                // Lock collateral UTXO so it shows as locked in wallet balance
                if mn.tier != types::MasternodeTier::Free {
                    if let Some(ref outpoint) = mn.collateral_outpoint {
                        let lock_height = blockchain.get_height();
                        if let Err(e) = consensus_engine.utxo_manager.lock_collateral(
                            outpoint.clone(),
                            mn.address.clone(),
                            lock_height,
                            mn.tier.collateral(),
                        ) {
                            tracing::warn!("⚠️ Failed to lock local collateral UTXO: {:?}", e);
                        } else {
                            tracing::info!(
                                "🔒 Locked collateral UTXO for {} tier",
                                format!("{:?}", mn.tier)
                            );
                        }
                    }
                }

                // Rebuild collateral locks for ALL known masternodes (not just local)
                // This restores in-memory locks lost across restarts
                {
                    let all_masternodes = registry.list_all().await;
                    let lock_height = blockchain.get_height();
                    let entries: Vec<_> = all_masternodes
                        .iter()
                        .filter(|info| info.masternode.address != mn.address) // Skip local (already locked above)
                        .filter_map(|info| {
                            info.masternode.collateral_outpoint.as_ref().map(|op| {
                                (
                                    op.clone(),
                                    info.masternode.address.clone(),
                                    lock_height,
                                    info.masternode.tier.collateral(),
                                )
                            })
                        })
                        .collect();
                    if !entries.is_empty() {
                        consensus_engine
                            .utxo_manager
                            .rebuild_collateral_locks(entries);
                    }
                }

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
                        tracing::debug!("📤 Broadcasting GetMasternodes to all peers");
                        peer_connection_registry_clone
                            .broadcast(NetworkMessage::GetMasternodes)
                            .await;
                    }
                }
            }
        });
        shutdown_manager.register_task(peer_exchange_handle);

        // Start masternode health monitoring and reconnection task
        let health_registry = registry.clone();
        let health_peer_manager = peer_manager.clone();
        let health_shutdown = shutdown_token.clone();
        let health_handle = tokio::spawn(async move {
            // Wait for peers to connect before first health check
            tokio::time::sleep(tokio::time::Duration::from_secs(120)).await;
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(120)); // Every 2 minutes
            loop {
                tokio::select! {
                    _ = health_shutdown.cancelled() => {
                        tracing::debug!("🛑 Health monitoring task shutting down gracefully");
                        break;
                    }
                    _ = interval.tick() => {
                        // Check network health
                        let health = health_registry.check_network_health().await;

                        match health.status {
                            crate::masternode_registry::HealthStatus::Critical => {
                                tracing::error!(
                                    "🚨 CRITICAL: {} active / {} total masternodes",
                                    health.active_masternodes,
                                    health.total_masternodes
                                );
                                for action in &health.actions_needed {
                                    tracing::error!("   → {}", action);
                                }
                            }
                            crate::masternode_registry::HealthStatus::Warning => {
                                tracing::warn!(
                                    "⚠️ WARNING: {} active / {} total masternodes",
                                    health.active_masternodes,
                                    health.total_masternodes
                                );
                                for action in &health.actions_needed {
                                    tracing::warn!("   → {}", action);
                                }
                            }
                            crate::masternode_registry::HealthStatus::Degraded => {
                                tracing::info!(
                                    "📊 Network degraded: {} active / {} total masternodes ({} inactive)",
                                    health.active_masternodes,
                                    health.total_masternodes,
                                    health.inactive_masternodes
                                );
                            }
                            crate::masternode_registry::HealthStatus::Healthy => {
                                tracing::debug!(
                                    "✓ Network healthy: {} active / {} total masternodes",
                                    health.active_masternodes,
                                    health.total_masternodes
                                );
                            }
                        }

                        // If unhealthy, attempt reconnection to inactive masternodes
                        if health.active_masternodes < 5 {
                            let inactive_addresses = health_registry.get_inactive_masternode_addresses().await;
                            if !inactive_addresses.is_empty() {
                                tracing::info!(
                                    "🔄 Attempting to reconnect to {} inactive masternodes",
                                    inactive_addresses.len()
                                );

                                for address in &inactive_addresses {
                                    let pm = health_peer_manager.clone();
                                    let addr = address.clone();
                                    tokio::spawn(async move {
                                        if pm.add_peer(addr.clone()).await {
                                            tracing::info!("   ✓ Reconnection attempt to {}", addr);
                                        } else {
                                            tracing::debug!("   ⚠️ Failed to reconnect to {}", addr);
                                        }
                                    });
                                }
                            }
                        }
                    }
                }
            }
        });
        shutdown_manager.register_task(health_handle);

        // Start masternode announcement task
        let mn_for_announcement = mn.clone();
        let peer_registry_for_announcement = peer_connection_registry.clone();

        let announcement_handle = tokio::spawn(async move {
            // Wait 10 seconds for initial peer connections
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            // Broadcast V3 announcement (certificate field kept empty for compat)
            let announcement = NetworkMessage::MasternodeAnnouncementV3 {
                address: mn_for_announcement.address.clone(),
                reward_address: mn_for_announcement.wallet_address.clone(),
                tier: mn_for_announcement.tier,
                public_key: mn_for_announcement.public_key,
                collateral_outpoint: mn_for_announcement.collateral_outpoint.clone(),
                certificate: vec![0u8; 64],
            };

            peer_registry_for_announcement
                .broadcast(announcement.clone())
                .await;
            tracing::info!("📢 Broadcast masternode announcement (V3) to network");

            // Continue broadcasting every 60 seconds to ensure visibility
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                peer_registry_for_announcement
                    .broadcast(announcement.clone())
                    .await;
                tracing::debug!("📢 Re-broadcast masternode announcement");
            }
        });
        shutdown_manager.register_task(announcement_handle);
    } else {
        // Non-masternode node: still rebuild collateral locks for known peers
        let all_masternodes = registry.list_all().await;
        let lock_height = blockchain.get_height();
        let entries: Vec<_> = all_masternodes
            .iter()
            .filter_map(|info| {
                info.masternode.collateral_outpoint.as_ref().map(|op| {
                    (
                        op.clone(),
                        info.masternode.address.clone(),
                        lock_height,
                        info.masternode.tier.collateral(),
                    )
                })
            })
            .collect();
        if !entries.is_empty() {
            consensus_engine
                .utxo_manager
                .rebuild_collateral_locks(entries);
        }
    }

    // Initialize blockchain and sync from peers in background
    let blockchain_init = blockchain.clone();
    let blockchain_server = blockchain_init.clone();
    let peer_registry_for_sync = peer_connection_registry.clone();
    let sync_complete_signal = sync_complete.clone();
    let bootstrap_registry = registry.clone();
    let genesis_external_ip = config.network.external_address.clone();

    let genesis_sync_handle = tokio::spawn(async move {
        // STEP 1: Verify existing blockchain or prepare for genesis
        tracing::info!("📥 Initializing blockchain...");
        if let Err(e) = blockchain_init.initialize_genesis().await {
            tracing::error!("❌ Failed to initialize blockchain: {}", e);
            return;
        }

        // STEP 2: If no genesis, try to obtain one
        if !blockchain_init.has_genesis() {
            tracing::info!("🌱 No genesis found - attempting to sync from network");

            // Phase 1: Wait for peers and try to sync genesis from network
            // This handles joining an existing network
            let mut sync_attempts = 0;
            const MAX_SYNC_ATTEMPTS: u32 = 3;
            const PEER_WAIT_SECS: u64 = 15;
            const GENESIS_WAIT_SECS: u64 = 20;

            while sync_attempts < MAX_SYNC_ATTEMPTS && !blockchain_init.has_genesis() {
                sync_attempts += 1;
                tracing::info!(
                    "📡 Sync attempt {}/{}: waiting {}s for peer connections...",
                    sync_attempts,
                    MAX_SYNC_ATTEMPTS,
                    PEER_WAIT_SECS
                );

                tokio::time::sleep(tokio::time::Duration::from_secs(PEER_WAIT_SECS)).await;

                let connected = peer_registry_for_sync.get_connected_peers().await;
                if connected.is_empty() {
                    tracing::info!("   No peers connected yet");
                    continue;
                }

                tracing::info!("📥 Requesting genesis from {} peer(s)", connected.len());

                // Request block 0 from all peers
                for peer_ip in &connected {
                    let msg = crate::network::message::NetworkMessage::GetBlocks(0, 0);
                    let _ = peer_registry_for_sync.send_to_peer(peer_ip, msg).await;
                }

                // Wait for genesis to arrive
                let mut wait_secs = 0;
                while wait_secs < GENESIS_WAIT_SECS {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    if blockchain_init.has_genesis() {
                        tracing::info!("✅ Successfully synced genesis block from network");
                        break;
                    }
                    wait_secs += 1;
                }
            }

            // Phase 2: If still no genesis, this is a new network - generate dynamically
            if !blockchain_init.has_genesis() {
                tracing::info!("🌱 No genesis on network - initiating dynamic generation");

                // Wait for masternodes to discover each other with exponential backoff
                // Start with 30s, then 60s, then 90s - total 180s max wait
                const DISCOVERY_ROUNDS: u32 = 3;
                const BASE_DISCOVERY_WAIT: u64 = 30;

                for round in 1..=DISCOVERY_ROUNDS {
                    if blockchain_init.has_genesis() {
                        break; // Genesis arrived while waiting
                    }

                    let wait_time = BASE_DISCOVERY_WAIT * round as u64;
                    tracing::info!(
                        "⏳ Discovery round {}/{}: waiting {}s for masternodes...",
                        round,
                        DISCOVERY_ROUNDS,
                        wait_time
                    );

                    tokio::time::sleep(tokio::time::Duration::from_secs(wait_time)).await;

                    // Check again if genesis arrived
                    if blockchain_init.has_genesis() {
                        tracing::info!("✅ Genesis block received during discovery wait");
                        break;
                    }

                    let registered = bootstrap_registry.get_all().await;
                    if registered.is_empty() {
                        tracing::warn!("   No masternodes registered yet (round {})", round);
                        continue;
                    }

                    tracing::info!(
                        "   {} masternodes discovered, proceeding with leader election",
                        registered.len()
                    );

                    // Sort masternodes deterministically by address
                    let mut sorted_mns = registered.clone();
                    sorted_mns.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));
                    let leader_address = sorted_mns[0].masternode.address.clone();

                    tracing::info!(
                        "🎲 Genesis leader election: {} masternodes, leader = {}",
                        sorted_mns.len(),
                        leader_address
                    );

                    // Check if we are the leader
                    let are_we_leader_by_config =
                        genesis_external_ip.as_deref() == Some(leader_address.as_str());
                    let are_we_leader_by_registry = bootstrap_registry
                        .get_local_masternode()
                        .await
                        .map(|mn| mn.masternode.address == leader_address)
                        .unwrap_or(false);
                    let are_we_leader = are_we_leader_by_config || are_we_leader_by_registry;

                    tracing::info!(
                        "🔍 Leader check: external_address={:?}, by_config={}, by_registry={}",
                        genesis_external_ip,
                        are_we_leader_by_config,
                        are_we_leader_by_registry
                    );

                    if are_we_leader {
                        // We are the leader - generate genesis
                        tracing::info!("👑 We are the genesis leader - generating genesis block");

                        // Double-check no genesis arrived in the meantime (prevent race)
                        if blockchain_init.has_genesis() {
                            tracing::info!("✅ Genesis arrived just before generation - using received genesis");
                            break;
                        }

                        if let Err(e) = blockchain_init.generate_dynamic_genesis().await {
                            tracing::error!("❌ Failed to generate genesis: {}", e);
                            continue; // Try next round
                        }

                        // Broadcast genesis to all peers
                        if let Ok(genesis) = blockchain_init.get_block_by_height(0).await {
                            tracing::info!("📤 Broadcasting genesis block to all peers");
                            let proposal =
                                crate::network::message::NetworkMessage::TimeLockBlockProposal {
                                    block: genesis,
                                };
                            peer_registry_for_sync.broadcast(proposal).await;
                        }
                        break;
                    } else {
                        // We are NOT the leader - wait for genesis from leader
                        // Use longer timeout and re-request periodically
                        tracing::info!("⏳ Waiting for genesis from leader ({})", leader_address);

                        const LEADER_WAIT_SECS: u64 = 45;
                        const REQUEST_INTERVAL: u64 = 10;
                        let mut waited = 0u64;

                        while waited < LEADER_WAIT_SECS && !blockchain_init.has_genesis() {
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            waited += 1;

                            // Re-request genesis periodically
                            if waited % REQUEST_INTERVAL == 0 {
                                let connected = peer_registry_for_sync.get_connected_peers().await;
                                for peer_ip in &connected {
                                    let msg =
                                        crate::network::message::NetworkMessage::GetBlocks(0, 0);
                                    let _ = peer_registry_for_sync.send_to_peer(peer_ip, msg).await;
                                }
                            }
                        }

                        if blockchain_init.has_genesis() {
                            tracing::info!("✅ Received genesis block from leader");
                            break;
                        }

                        // Only generate fallback on LAST round to prevent race conditions
                        if round == DISCOVERY_ROUNDS {
                            tracing::warn!(
                                "⚠️  Leader timeout after {} rounds - generating fallback genesis",
                                DISCOVERY_ROUNDS
                            );

                            // Final check before fallback generation
                            if blockchain_init.has_genesis() {
                                tracing::info!("✅ Genesis arrived just before fallback - using received genesis");
                                break;
                            }

                            if let Err(e) = blockchain_init.generate_dynamic_genesis().await {
                                tracing::error!("❌ Failed to generate fallback genesis: {}", e);
                            } else if let Ok(genesis) = blockchain_init.get_block_by_height(0).await
                            {
                                tracing::info!("📤 Broadcasting fallback genesis block");
                                let proposal =
                                    crate::network::message::NetworkMessage::TimeLockBlockProposal {
                                        block: genesis,
                                    };
                                peer_registry_for_sync.broadcast(proposal).await;
                            }
                        }
                    }
                }
            }
        } else {
            tracing::info!(
                "✓ Genesis block exists (height: {})",
                blockchain_init.get_height()
            );
        }

        // Final verification
        if !blockchain_init.has_genesis() {
            tracing::error!(
                "❌ Failed to obtain genesis block after all attempts - cannot proceed"
            );
            tracing::error!("   Ensure at least one masternode is registered and reachable");
            return;
        }

        tracing::info!("✓ Genesis block ready, now syncing remaining blocks from peers");

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

        // STEP 2.5: Actively request chain tips from all peers BEFORE making any sync decisions.
        // This ensures we have fresh data instead of relying on stale/empty cache.
        // Without this, restarted nodes may see empty peer caches and incorrectly enter bootstrap mode.
        {
            let connected = peer_registry_for_sync.get_connected_peers().await;
            if !connected.is_empty() {
                tracing::info!(
                    "📡 Requesting chain tips from {} peer(s) for fresh sync data",
                    connected.len()
                );
                for peer_ip in &connected {
                    let msg = crate::network::message::NetworkMessage::GetChainTip;
                    let _ = peer_registry_for_sync.send_to_peer(peer_ip, msg).await;
                }
                // Wait briefly for chain tip responses to arrive and be processed
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                tracing::info!("✓ Chain tip request round complete");
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
                            // Auto-heal: re-fetch corrupt blocks from peers
                            match blockchain_for_integrity
                                .repair_corrupt_blocks(&corrupt_blocks)
                                .await
                            {
                                Ok(repaired) => {
                                    tracing::info!(
                                        "🔧 Auto-healing: repaired {}/{} corrupt blocks from peers",
                                        repaired,
                                        corrupt_blocks.len()
                                    );
                                }
                                Err(e) => {
                                    tracing::error!("❌ Failed to repair corrupt blocks: {}", e);
                                }
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
    shutdown_manager.register_task(genesis_sync_handle);

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
    let block_utxo_mgr = utxo_mgr.clone(); // For draining pending collateral unlocks
    let block_blockchain = blockchain.clone();
    let block_peer_registry = peer_connection_registry.clone(); // Used for peer sync before fallback
    let block_masternode_address = masternode_address.clone(); // For leader comparison
    let shutdown_token_block = shutdown_token.clone();
    let block_consensus_engine = consensus_engine.clone(); // For TimeLock voting

    // Guard flag to prevent duplicate block production (P2P best practice #8)
    let is_producing_block = Arc::new(AtomicBool::new(false));
    let is_producing_block_clone = is_producing_block.clone();

    // Trigger for immediate block production (when status check detects chain is behind)
    let production_trigger = Arc::new(tokio::sync::Notify::new());
    let production_trigger_producer = production_trigger.clone();

    let block_production_handle = tokio::spawn(async move {
        let is_producing = is_producing_block_clone;

        // CRITICAL: Wait for genesis block before starting block production
        // Without genesis, we cannot produce any blocks (block 1 needs block 0's hash)
        let mut genesis_wait = 0;
        const MAX_GENESIS_WAIT_SECS: u64 = 300; // 5 minutes max wait for genesis
        while !block_blockchain.has_genesis() && genesis_wait < MAX_GENESIS_WAIT_SECS {
            if genesis_wait % 30 == 0 {
                tracing::info!(
                    "⏳ Waiting for genesis block before starting block production ({}s elapsed)...",
                    genesis_wait
                );
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            genesis_wait += 1;
        }

        if !block_blockchain.has_genesis() {
            tracing::error!(
                "❌ No genesis block after {}s - cannot start block production",
                MAX_GENESIS_WAIT_SECS
            );
            return;
        }

        tracing::info!("✅ Genesis block ready - starting block production loop");

        // SYNC GATE: Before producing any blocks, ensure we have fresh peer data.
        // If we're significantly behind expected height, we MUST sync first.
        // This prevents restarted nodes from entering bootstrap mode and forking.
        let gate_height = block_blockchain.get_height();
        let gate_expected = block_blockchain.calculate_expected_height();
        let gate_behind = gate_expected.saturating_sub(gate_height);

        if gate_behind > 2 {
            tracing::info!(
                "🔒 Sync gate: {} blocks behind expected height ({} vs {}) - waiting for fresh peer data before block production",
                gate_behind, gate_height, gate_expected
            );

            // Wait for at least one peer to report a chain tip (confirms fresh data, not stale cache)
            let mut gate_wait = 0u64;
            const MAX_GATE_WAIT_SECS: u64 = 60; // Wait up to 60 seconds for peer data
            const MIN_CONFIRMED_PEERS: usize = 1;

            loop {
                if gate_wait >= MAX_GATE_WAIT_SECS {
                    tracing::warn!(
                        "⚠️ Sync gate timeout after {}s - proceeding with caution",
                        gate_wait
                    );
                    break;
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                gate_wait += 2;

                // Check if any peer has reported a height via pong or chain tip
                let peers = block_peer_registry.get_compatible_peers().await;
                let mut peers_with_data = 0usize;
                let mut max_reported_height = 0u64;
                for peer_ip in &peers {
                    if let Some(h) = block_peer_registry.get_peer_height(peer_ip).await {
                        peers_with_data += 1;
                        if h > max_reported_height {
                            max_reported_height = h;
                        }
                    } else if let Some((h, _)) =
                        block_peer_registry.get_peer_chain_tip(peer_ip).await
                    {
                        peers_with_data += 1;
                        if h > max_reported_height {
                            max_reported_height = h;
                        }
                    }
                }

                if peers_with_data >= MIN_CONFIRMED_PEERS {
                    tracing::info!(
                        "🔓 Sync gate passed: {} peer(s) reporting data, max height {} (waited {}s)",
                        peers_with_data, max_reported_height, gate_wait
                    );

                    // If peers have a longer chain, request sync before proceeding
                    if max_reported_height > block_blockchain.get_height() {
                        tracing::info!(
                            "📥 Sync gate: peers ahead at height {} - requesting sync before production",
                            max_reported_height
                        );
                        for peer_ip in &peers {
                            let current = block_blockchain.get_height();
                            let msg = crate::network::message::NetworkMessage::GetBlocks(
                                current + 1,
                                max_reported_height.min(current + 50),
                            );
                            let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                        }
                        // Give sync some time to process incoming blocks
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                    break;
                }

                if gate_wait % 10 == 0 {
                    tracing::info!(
                        "⏳ Sync gate: waiting for peer data ({}/{}s, {} peers connected, {} with data)",
                        gate_wait, MAX_GATE_WAIT_SECS, peers.len(), peers_with_data
                    );
                }
            }
        } else {
            // Not far behind - short delay for sync to settle
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }

        // Time-based production trigger: Check if we're behind schedule
        // When behind expected height, normal consensus produces blocks rapidly
        let current_height = block_blockchain.get_height();
        let expected_height = block_blockchain.calculate_expected_height();
        let blocks_behind = expected_height.saturating_sub(current_height);

        let genesis_timestamp = block_blockchain.genesis_timestamp();
        let now_timestamp = chrono::Utc::now().timestamp();

        // Calculate when the current expected block should have been produced
        let expected_block_time = genesis_timestamp + (expected_height as i64 * 600);
        let time_since_expected = now_timestamp - expected_block_time;

        // Smart initial wait:
        // - If many blocks behind (>2): Start immediately — consensus drives rapid production
        // - If few blocks behind (1-2): Use 5-minute grace period for normal schedule
        let grace_period = 300; // 5 minutes in seconds

        let initial_wait = if blocks_behind > 2 {
            // More than 2 blocks behind - start producing immediately via normal consensus
            tracing::info!(
                "⚡ {} blocks behind - starting immediate block production (>2 blocks threshold)",
                blocks_behind
            );
            0
        } else if blocks_behind > 0 && time_since_expected >= grace_period {
            // 1-2 blocks behind AND 5+ minutes past when block should have been produced
            tracing::info!(
                "⚡ {} blocks behind, {}s past expected block time - starting immediate production",
                blocks_behind,
                time_since_expected
            );
            0
        } else if blocks_behind > 0 {
            // 1 block behind and within the 5-minute grace period
            let remaining_grace = grace_period - time_since_expected;
            tracing::info!(
                "⏳ {} blocks behind but only {}s past expected time - waiting {}s before production",
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

        // Wait until the appropriate time (or start immediately if behind)
        if initial_wait > 0 {
            tokio::time::sleep(tokio::time::Duration::from_secs(initial_wait as u64)).await;
        }

        // Use a short interval (1 second) and check timing internally
        // This allows rapid production when behind while still respecting 10-minute marks when synced
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_block_period_started: u64 = 0; // Track which block period we've started

        // Event-driven: wake up immediately when any block is added to our chain
        // (from peer sync, consensus finalization, or our own production)
        let block_signal = block_blockchain.block_added_signal();

        // Leader rotation timeout tracking
        // If a leader doesn't produce within LEADER_TIMEOUT_SECS, rotate to next leader
        const LEADER_TIMEOUT_SECS: u64 = 10; // Wait 10s before rotating to backup leader (2x block production time)
        let mut waiting_for_height: Option<u64> = None;
        let mut leader_attempt: u64 = 0; // Increments when leader times out
        let mut height_first_seen = std::time::Instant::now();

        // CRITICAL: Periodic GetChainTip requests to keep peer_chain_tips cache fresh
        // This ensures block production can always verify consensus on peer heights
        let mut last_chain_tip_request = std::time::Instant::now();
        const CHAIN_TIP_REQUEST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

        // Fork sync retry cooldown: avoid spamming when blocks repeatedly fail validation
        let mut last_sync_attempt_height: u64 = 0;
        let mut sync_attempt_count: u32 = 0;
        let mut last_sync_attempt_time = std::time::Instant::now();

        loop {
            tokio::select! {
                _ = shutdown_token_block.cancelled() => {
                    tracing::debug!("🛑 Block production task shutting down gracefully");
                    break;
                }
                _ = production_trigger_producer.notified() => {
                    // Triggered by status check — immediate re-evaluation
                    tracing::info!("🔔 Block production triggered by status check");
                }
                _ = block_signal.notified() => {
                    // A block was added (from peer or self) - immediately re-evaluate
                    tracing::debug!("🔔 Block added signal - re-evaluating production");
                }
                _ = interval.tick() => {
                    // Regular 1-second check (fallback for leader timeout, chain tip refresh)
                }
            }

            // CRITICAL BOOTSTRAP FIX: Periodically request chain tips from peers
            // This keeps peer_chain_tips cache fresh so block production can verify consensus
            // Without this, nodes get stuck at bootstrap because check_2_3_consensus_for_production()
            // has no peer data to work with
            if last_chain_tip_request.elapsed() >= CHAIN_TIP_REQUEST_INTERVAL {
                let connected = block_peer_registry.get_connected_peers().await;
                if !connected.is_empty() {
                    tracing::debug!(
                        "📡 Periodic chain tip refresh: requesting from {} peer(s)",
                        connected.len()
                    );
                    block_peer_registry
                        .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                        .await;
                    last_chain_tip_request = std::time::Instant::now();
                }
            }

            // Mark start of new block period (only once per period)
            let current_height = block_blockchain.get_height();
            block_registry.update_height(current_height);

            // Drain any pending collateral unlocks queued by cleanup tasks
            let unlocked = block_registry.drain_pending_unlocks(&block_utxo_mgr);
            if unlocked > 0 {
                tracing::info!("🔓 Unlocked {} stale collateral(s)", unlocked);
            }

            let expected_period = current_height + 1;
            if expected_period > last_block_period_started {
                block_registry.start_new_block_period().await;
                last_block_period_started = expected_period;
            }

            let expected_height = block_blockchain.calculate_expected_height();
            let blocks_behind = expected_height.saturating_sub(current_height);

            // Early time gate: skip expensive masternode selection when next block isn't due yet
            // This prevents noisy fallback logging every second while waiting for the next slot
            // CRITICAL: Also prevents producing blocks with future timestamps —
            // receiving nodes reject blocks with timestamp > now + 60s tolerance
            {
                let next_h = current_height + 1;
                let genesis_ts = block_blockchain.genesis_timestamp();
                let now_ts = chrono::Utc::now().timestamp();
                let scheduled = genesis_ts + (next_h as i64 * 600);
                let tolerance = constants::blockchain::TIMESTAMP_TOLERANCE_SECS;
                if now_ts + tolerance < scheduled {
                    if blocks_behind >= 5 {
                        tracing::debug!(
                            "⏳ Production paused: block {} scheduled at {} ({}s in future, tolerance {}s)",
                            next_h, scheduled, scheduled - now_ts, tolerance
                        );
                    } else {
                        tracing::debug!(
                            "📅 Block {} not due for {}s (early gate)",
                            next_h,
                            scheduled - now_ts
                        );
                    }
                    continue;
                }
            }

            // Get masternodes eligible for leader selection and rewards
            // CRITICAL: This MUST use the SAME logic as blockchain.rs produce_block_at_height()
            // to ensure selected leader is eligible for rewards (prevents participation chain break)
            let is_bootstrap = current_height == 0; // Only block 1 (height 0→1) uses bootstrap

            let eligible = if is_bootstrap {
                let all_nodes = block_registry.get_all_for_bootstrap().await;
                tracing::info!(
                    "🌱 Bootstrap mode (height {}): using ALL {} registered masternodes (including inactive, no bitmap yet)",
                    current_height,
                    all_nodes.len()
                );
                // At height 0 (producing block 1), use ALL registered masternodes
                // After block 1, the bitmap from block 1 will be used for block 2
                all_nodes
            } else {
                // Normal mode: use participation-based selection from previous block's bitmap
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

            tracing::debug!(
                "📋 Got {} eligible masternodes before fallback checks",
                masternodes.len()
            );

            // DEADLOCK PREVENTION: Progressive fallback when insufficient masternodes
            // 1. First try: eligible masternodes (from bitmap/participation)
            // 2. If < 3: fallback to ALL active masternodes
            // 3. If still < 3: emergency fallback to ALL registered (including inactive)
            if masternodes.len() < 3 {
                tracing::debug!(
                    "📊 Only {} eligible masternodes from bitmap (need 3) - using participation fallback",
                    masternodes.len()
                );
                let active_infos = block_registry
                    .get_masternodes_for_rewards(&block_blockchain)
                    .await;
                masternodes = active_infos
                    .iter()
                    .map(|info| info.masternode.clone())
                    .collect();

                // CRITICAL: If still insufficient, REFUSE to produce blocks (fork prevention)
                if masternodes.len() < 3 {
                    // Rate-limit this error (once per 60s)
                    static LAST_FORK_WARN: std::sync::atomic::AtomicI64 =
                        std::sync::atomic::AtomicI64::new(0);
                    let now_secs = chrono::Utc::now().timestamp();
                    let last = LAST_FORK_WARN.load(Ordering::Relaxed);
                    if now_secs - last >= 60 {
                        LAST_FORK_WARN.store(now_secs, Ordering::Relaxed);
                        tracing::error!(
                            "🛡️ FORK PREVENTION: Only {} active masternodes (minimum 3 required) - refusing block production",
                            masternodes.len()
                        );
                    }
                    continue;
                }
            }

            // Double-check we have enough masternodes after fallback logic
            if masternodes.len() < 3 {
                tracing::warn!(
                    "⚠️ Insufficient masternodes ({}) for block production - skipping",
                    masternodes.len()
                );
                continue;
            }

            // Additional safety: check masternodes is not empty to prevent panic
            if masternodes.is_empty() {
                tracing::error!(
                    "🛡️ FORK PREVENTION: Empty masternode set - refusing block production"
                );
                continue;
            }

            // Sort deterministically by address for consistent leader election across all nodes
            sort_masternodes_canonical(&mut masternodes);

            // Anti-sybil: filter immature Free-tier nodes from VRF sortition.
            // Done after fallback logic so the maturity gate doesn't interfere with
            // the "minimum 3 masternodes" threshold (paid tiers always pass).
            {
                let mut vrf_eligible = Vec::with_capacity(masternodes.len());
                for mn in masternodes.iter() {
                    if block_registry
                        .is_address_vrf_eligible(&mn.address, current_height)
                        .await
                    {
                        vrf_eligible.push(mn.clone());
                    }
                }
                if vrf_eligible.len() >= 3 {
                    masternodes = vrf_eligible;
                }
                // If filtering would drop below 3, keep all (safety: don't block production)
            }

            // Calculate time-based values for block production
            let genesis_timestamp = block_blockchain.genesis_timestamp();
            let now_timestamp = chrono::Utc::now().timestamp();

            // Require minimum masternodes for production after all fallback attempts
            // If still less than 3, skip block production
            if masternodes.len() < 3 {
                // Log periodically (every 60s) to avoid spam
                static LAST_WARN: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last_warn = LAST_WARN.load(Ordering::Relaxed);
                if now_secs - last_warn >= 60 {
                    LAST_WARN.store(now_secs, Ordering::Relaxed);
                    tracing::error!(
                        "🚨 CRITICAL: Cannot produce block - only {} registered masternodes (minimum 3 required). Height: {}, Expected: {}",
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
            // 5. Use TimeLock/TimeVote consensus for leader election
            // ═══════════════════════════════════════════════════════════════════════════════

            let next_height = current_height + 1;
            let next_block_scheduled_time = genesis_timestamp + (next_height as i64 * 600); // 600 seconds (10 min) per block
            let time_past_scheduled = now_timestamp - next_block_scheduled_time;

            // Sync threshold: if more than this many blocks behind, try to sync first
            const SYNC_THRESHOLD_BLOCKS: u64 = 5;

            // Case 1: Next block not due yet - wait until scheduled time
            // When far behind (>5 blocks), skip time gate so consensus can produce rapidly
            if time_past_scheduled < 0 && blocks_behind < SYNC_THRESHOLD_BLOCKS {
                let wait_secs = -time_past_scheduled;
                tracing::debug!("📅 Block {} not due for {}s", next_height, wait_secs);
                continue;
            }

            // Rapid production: When far behind, normal consensus produces as fast as it allows
            if blocks_behind >= SYNC_THRESHOLD_BLOCKS {
                tracing::debug!(
                    "⚡ {} blocks behind, producing rapidly via normal consensus",
                    blocks_behind
                );
            }

            // Case 2: Way behind - try to sync first before producing
            // BUT: Check if we're in a bootstrap scenario (everyone at same height)
            if blocks_behind >= SYNC_THRESHOLD_BLOCKS {
                tracing::debug!(
                    "🔄 {} blocks behind - checking if peers have blocks to sync",
                    blocks_behind
                );

                // Use only compatible peers for sync (excludes nodes on incompatible chains)
                let connected_peers = block_peer_registry.get_compatible_peers().await;

                // CRITICAL: If sync coordinator is already syncing, check if it's making progress
                // Use event-driven approach: check status and loop back immediately
                if block_blockchain.is_syncing() {
                    // Check if bootstrap - all peers CONFIRMED at height 0
                    // CRITICAL: Require positive confirmation from peers, not just absence of data.
                    // Peers with no cached chain tip are "unknown", NOT "at zero".
                    let mut confirmed_at_zero = 0u32;
                    let mut confirmed_higher = 0u32;
                    for peer_ip in &connected_peers {
                        if let Some((height, _)) =
                            block_peer_registry.get_peer_chain_tip(peer_ip).await
                        {
                            if height > 0 {
                                confirmed_higher += 1;
                            } else {
                                confirmed_at_zero += 1;
                            }
                        }
                        // Peers with no cached tip are not counted at all (unknown state)
                    }
                    let all_confirmed_at_zero = confirmed_at_zero >= 3 && confirmed_higher == 0;

                    // CRITICAL FIX: Don't override if we're significantly behind expected height
                    // If blocks_behind > 10, peers might actually have blocks - trust time-based height, not cached tips
                    let can_bootstrap_override = all_confirmed_at_zero
                        && current_height == 0
                        && connected_peers.len() >= 3
                        && blocks_behind <= 10;

                    if can_bootstrap_override {
                        tracing::warn!("🚨 Bootstrap override: {} peers confirmed at height 0 (of {} connected), sync is stuck - forcing block production", confirmed_at_zero, connected_peers.len());
                        // Fall through to production logic - skip consensus check entirely
                        // Everyone is at height 0, no blocks to sync, time to produce genesis+1
                    } else {
                        tracing::debug!("⏳ Sync coordinator is syncing - checking again shortly (blocks_behind: {})", blocks_behind);
                        continue; // Loop back immediately via 1-second interval
                    }
                } else if !connected_peers.is_empty() {
                    // Not in syncing state - check consensus to decide sync vs produce
                    // Single consensus check handles both sync-behind and same-height fork cases
                    let min_peers_for_check = connected_peers.len().min(3);
                    if connected_peers.len() >= min_peers_for_check {
                        if let Some((consensus_height, _)) =
                            block_blockchain.compare_chain_with_peers().await
                        {
                            // Some() means peers are ahead or there's a fork we should switch to.
                            // compare_chain_with_peers() only returns Some when action is needed.
                            if consensus_height > current_height {
                                // Cooldown: if we've been failing to sync the same height, back off
                                if consensus_height == last_sync_attempt_height {
                                    sync_attempt_count += 1;
                                    if sync_attempt_count > 3
                                        && last_sync_attempt_time.elapsed()
                                            < std::time::Duration::from_secs(30)
                                    {
                                        tracing::debug!(
                                            "⏳ Sync retry cooldown: height {} failed {} times, waiting 30s",
                                            consensus_height,
                                            sync_attempt_count
                                        );
                                        tokio::time::sleep(tokio::time::Duration::from_secs(30))
                                            .await;
                                    }
                                } else {
                                    last_sync_attempt_height = consensus_height;
                                    sync_attempt_count = 1;
                                }
                                last_sync_attempt_time = std::time::Instant::now();

                                // Consensus is ahead of us - request blocks and loop back
                                tracing::debug!(
                                    "Peers at height {} (we're at {}) - requesting blocks",
                                    consensus_height,
                                    current_height
                                );

                                let probe_start = current_height + 1;
                                let probe_end = consensus_height.min(current_height + 50);

                                for peer_ip in &connected_peers {
                                    let msg = NetworkMessage::GetBlocks(probe_start, probe_end);
                                    let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                                }
                                continue;
                            }
                            // consensus_height == current_height with Some = same-height fork
                            // We're on the minority chain — sync to majority before producing
                            tracing::warn!(
                                "🔀 Fork detected at height {}: syncing to majority chain before producing",
                                current_height
                            );
                            if let Err(e) = block_blockchain.sync_from_peers(None).await {
                                tracing::warn!("⚠️  Sync to majority failed: {}", e);
                            }
                            continue;
                        }
                        // None means all peers agree on our chain (same height, same hash).
                        // This is a POSITIVE confirmation — safe to proceed to block production.
                        tracing::debug!(
                            "Peers agree at height {} - proceeding to production",
                            current_height
                        );
                    }
                } else {
                    // No compatible peers available
                    if blocks_behind > 10 {
                        tracing::warn!(
                            "⚠️  {} blocks behind but no peers available - waiting",
                            blocks_behind
                        );
                        continue;
                    }
                    tracing::warn!("⚠️  No peers available for sync - proceeding to production");
                }
            }

            // CRITICAL: Even when NOT far behind (< SYNC_THRESHOLD), verify peers
            // agree with our chain before producing. Without this, a node that missed
            // 1-2 blocks produces on a stale tip instead of syncing, creating a fork.
            if blocks_behind < SYNC_THRESHOLD_BLOCKS {
                let peers = block_peer_registry.get_compatible_peers().await;
                if !peers.is_empty() {
                    if let Some((consensus_height, _)) =
                        block_blockchain.compare_chain_with_peers().await
                    {
                        if consensus_height > current_height {
                            // Cooldown: if we've been failing to sync the same height, back off
                            if consensus_height == last_sync_attempt_height {
                                sync_attempt_count += 1;
                                if sync_attempt_count > 3
                                    && last_sync_attempt_time.elapsed()
                                        < std::time::Duration::from_secs(30)
                                {
                                    tracing::debug!(
                                        "⏳ Sync retry cooldown: height {} failed {} times, waiting 30s",
                                        consensus_height,
                                        sync_attempt_count
                                    );
                                    tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                                }
                            } else {
                                last_sync_attempt_height = consensus_height;
                                sync_attempt_count = 1;
                            }
                            last_sync_attempt_time = std::time::Instant::now();

                            tracing::warn!(
                                "🛡️ FORK PREVENTION: Peers at height {} > our {} - syncing instead of producing",
                                consensus_height,
                                current_height
                            );
                            let probe_start = current_height + 1;
                            let probe_end = consensus_height.min(current_height + 50);
                            for peer_ip in &peers {
                                let msg = NetworkMessage::GetBlocks(probe_start, probe_end);
                                let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                            }
                            continue;
                        }
                        // Same height but different hash = fork, sync to majority
                        tracing::warn!(
                            "🔀 Fork detected at height {}: syncing to majority chain before producing",
                            current_height
                        );
                        if let Err(e) = block_blockchain.sync_from_peers(None).await {
                            tracing::warn!("⚠️  Sync to majority failed: {}", e);
                        }
                        continue;
                    }
                    // None = peers agree with us — safe to produce
                }
            }

            // Case 3: Within grace period or sync failed - time to produce
            // Use TimeLock consensus for leader election

            // Get previous block hash for leader selection
            let prev_block_hash = match block_blockchain.get_block_hash(current_height) {
                Ok(hash) => hash,
                Err(e) => {
                    tracing::error!("Failed to get previous block hash: {}", e);
                    continue;
                }
            };

            // VRF timeout tracking: use min(slot_elapsed, wall_clock) to compute
            // relaxation attempts. This ensures:
            // - At tip: slot and wall clock agree → matches validator's slot-based check
            // - During catch-up: wall clock is small (just started watching this height),
            //   capping relaxation to prevent every node becoming eligible at once
            if waiting_for_height != Some(next_height) {
                waiting_for_height = Some(next_height);
                leader_attempt = 0;
                height_first_seen = std::time::Instant::now();
            }
            let slot_elapsed_secs = time_past_scheduled.max(0) as u64;
            let wall_elapsed_secs = height_first_seen.elapsed().as_secs();
            let effective_elapsed = slot_elapsed_secs.min(wall_elapsed_secs);
            let expected_attempt = effective_elapsed / LEADER_TIMEOUT_SECS;
            if expected_attempt > leader_attempt {
                leader_attempt = expected_attempt;
                if leader_attempt > 0 {
                    tracing::warn!(
                        "⏱️  No block for height {} after {}s waiting (attempt {}) — relaxing VRF threshold",
                        next_height,
                        effective_elapsed,
                        leader_attempt
                    );
                }
            }

            // VRF-based self-selection (Algorand-style sortition, §9.2)
            // Each node evaluates VRF with their own secret key to determine eligibility.
            // Only the node itself knows if it's selected until it reveals the VRF proof.
            let signing_key = match block_consensus_engine.get_signing_key() {
                Some(key) => key,
                None => {
                    tracing::debug!("⏸️  No signing key available for VRF evaluation");
                    continue;
                }
            };

            // Compute our VRF output for this slot
            let (_vrf_proof, _vrf_output, vrf_score) =
                crate::block::vrf::generate_block_vrf(&signing_key, next_height, &prev_block_hash);

            // Find our masternode in the eligible set
            let our_addr = match &block_masternode_address {
                Some(addr) => addr.clone(),
                None => continue,
            };
            let our_mn = match masternodes.iter().find(|mn| mn.address == our_addr) {
                Some(mn) => mn,
                None => {
                    tracing::debug!("⏸️  Our masternode not in eligible set");
                    continue;
                }
            };

            // Calculate sampling weights with fairness bonus
            // Fairness bonus: +1 per 10 blocks without reward, capped at +20
            // This ensures nodes that haven't produced blocks get increasing priority
            let blocks_without_reward_map = block_registry
                .get_verifiable_reward_tracking(&block_blockchain)
                .await;

            let our_blocks_without = blocks_without_reward_map
                .get(&our_addr)
                .copied()
                .unwrap_or(0);
            let our_fairness_bonus = (our_blocks_without / 10).min(20);
            let our_sampling_weight = {
                let raw = our_mn.tier.sampling_weight() + our_fairness_bonus;
                // Cap Free tier effective weight below Bronze base to prevent
                // zero-collateral nodes from outcompeting paid tiers via fairness bonus
                if matches!(our_mn.tier, crate::types::MasternodeTier::Free) {
                    raw.min(crate::types::MasternodeTier::Bronze.sampling_weight() - 1)
                } else {
                    raw
                }
            };

            let total_sampling_weight: u64 = masternodes
                .iter()
                .map(|mn| {
                    let bonus = blocks_without_reward_map
                        .get(&mn.address)
                        .copied()
                        .map(|b| (b / 10).min(20))
                        .unwrap_or(0);
                    let raw = mn.tier.sampling_weight() + bonus;
                    // Apply same Free-tier cap for total weight calculation
                    if matches!(mn.tier, crate::types::MasternodeTier::Free) {
                        raw.min(crate::types::MasternodeTier::Bronze.sampling_weight() - 1)
                    } else {
                        raw
                    }
                })
                .sum();

            // Apply threshold relaxation for timeout: multiply effective weight by 2^attempt
            // attempt=0: normal threshold, attempt=1: 2x more likely, attempt=2: 4x, etc.
            // SECURITY: Free tier nodes only get emergency boost after extended deadlock
            // (attempt >= 6 = 60s) to maintain sybil resistance while preventing permanent stalls.
            let effective_sampling_weight = if leader_attempt > 0 {
                let allow_boost = if matches!(our_mn.tier, crate::types::MasternodeTier::Free) {
                    leader_attempt >= 6 // Free tier: only after 60s deadlock
                } else {
                    true // Paid tiers: immediate relaxation
                };
                if allow_boost {
                    let multiplier = 1u64 << leader_attempt.min(20); // Cap to prevent overflow
                    our_sampling_weight
                        .saturating_mul(multiplier)
                        .min(total_sampling_weight)
                } else {
                    our_sampling_weight
                }
            } else {
                our_sampling_weight
            };

            let is_eligible = crate::block::vrf::vrf_check_proposer_eligible(
                vrf_score,
                effective_sampling_weight,
                total_sampling_weight,
            );

            // Log VRF evaluation periodically or on eligibility
            static LAST_VRF_LOG: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let now_secs = chrono::Utc::now().timestamp();
            let last_log = LAST_VRF_LOG.load(Ordering::Relaxed);
            if is_eligible || now_secs - last_log >= 30 {
                LAST_VRF_LOG.store(now_secs, Ordering::Relaxed);
                tracing::info!(
                    "🎲 Block {} VRF sortition: score={}, weight={}/{}, eligible: {}",
                    next_height,
                    vrf_score,
                    our_sampling_weight,
                    total_sampling_weight,
                    if is_eligible { "YES" } else { "NO" },
                );
            }

            if !is_eligible {
                tracing::debug!(
                    "⏸️  VRF score {} not below threshold for block {} (weight: {}/{})",
                    vrf_score,
                    next_height,
                    our_sampling_weight,
                    total_sampling_weight,
                );
                continue;
            }

            // We are VRF-eligible to propose!
            tracing::info!(
                "🎯 VRF selected as block proposer for height {} (score: {}, {}s past scheduled time)",
                next_height,
                vrf_score,
                time_past_scheduled
            );

            // RACE CONDITION PREVENTION: Check if a block proposal at this height
            // was already received (or produced by us) and cached. If a peer's cached
            // proposal has a strictly better VRF score, skip production — our vote is
            // already committed to that proposal and producing a competing block causes forks.
            // If it's our own cached proposal, skip silently (already produced & broadcast).
            let (_, vrf_block_cache_opt, _) = block_peer_registry.get_timelock_resources().await;
            if let Some(ref cache) = vrf_block_cache_opt {
                if let Some(existing) = cache.get_by_height(next_height) {
                    if existing.header.vrf_score > 0 {
                        if existing.header.leader == our_addr {
                            // Our own cached proposal — already produced and broadcast
                            tracing::debug!(
                                "⏭️  Already proposed block for height {}, waiting for consensus",
                                next_height,
                            );
                            continue;
                        } else if existing.header.vrf_score < vrf_score {
                            // A peer's proposal has a strictly better (lower) VRF score
                            tracing::info!(
                                "⏭️  Skipping block production for height {}: peer proposal has better VRF score ({} < {})",
                                next_height,
                                existing.header.vrf_score,
                                vrf_score
                            );
                            continue;
                        }
                    }
                }
            }

            // Use our own identity for block production
            let selected_producer = our_mn;

            // Safety checks before producing
            // Always require at least 3 peers to prevent isolated nodes from creating forks
            // We need network consensus to produce valid blocks
            let connected_peers = block_peer_registry.get_compatible_peers().await;
            let min_peers_required = 3;
            if connected_peers.len() < min_peers_required {
                tracing::warn!(
                    "⚠️ Only {} peer(s) connected - waiting for more peers before producing",
                    connected_peers.len()
                );
                continue;
            }

            // CRITICAL: Final check - verify peers don't have a longer chain
            // This prevents emergency mode from creating forks when network has progressed
            let mut max_peer_height_final = current_height;
            for peer_ip in &connected_peers {
                if let Some(h) = block_peer_registry.get_peer_height(peer_ip).await {
                    if h > max_peer_height_final {
                        max_peer_height_final = h;
                    }
                }
            }
            if max_peer_height_final > current_height {
                tracing::debug!(
                    "🛡️ Fork prevention: peers have height {} > our height {} - syncing instead of producing",
                    max_peer_height_final,
                    current_height
                );
                for peer_ip in &connected_peers {
                    let msg = NetworkMessage::GetBlocks(
                        current_height + 1,
                        max_peer_height_final.min(current_height + 50),
                    );
                    let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                }
                continue;
            }

            // CRITICAL: Check if block already exists in chain
            // This prevents producing a block that's already finalized
            // Note: We don't check the cache because proposals may timeout/fail
            // and we need to allow retry. TimeLock consensus voting prevents duplicates.
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

            // Wait for majority peer consensus before producing (event-driven).
            // After receiving a block from a peer, our height advances but peers
            // may still report their old height. Instead of failing with "no majority
            // consensus" and retrying every second, wait for peer chain tips to update.
            if !block_blockchain.check_2_3_consensus_cached().await {
                block_blockchain.invalidate_consensus_cache().await;
                block_peer_registry
                    .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                    .await;

                let tip_signal = block_peer_registry.chain_tip_updated_signal();
                let consensus_ok =
                    tokio::time::timeout(std::time::Duration::from_secs(10), async {
                        loop {
                            tip_signal.notified().await;
                            // Re-check with fresh data (invalidate stale cache each time)
                            block_blockchain.invalidate_consensus_cache().await;
                            if block_blockchain.check_2_3_consensus_cached().await {
                                return true;
                            }
                        }
                    })
                    .await;

                if consensus_ok != Ok(true) {
                    tracing::warn!(
                        "⏱️ No majority peer consensus for block {} after 10s — skipping this attempt",
                        next_height
                    );
                    is_producing.store(false, Ordering::SeqCst);
                    continue;
                }
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

                    // TimeLock Consensus Flow:
                    // 1. Cache block locally for finalization
                    // 2. Broadcast TimeLockBlockProposal to all peers (NOT add to chain yet)
                    // 3. All nodes (including us) validate and vote
                    // 4. When >50% prepare votes → precommit phase
                    // 5. When >50% precommit votes → block finalized, all add to chain

                    // Step 1: Cache the block for finalization (leader must also cache)
                    let (_, block_cache_opt, _) =
                        block_peer_registry.get_timelock_resources().await;
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
                            Some(info) => info.masternode.tier.sampling_weight().max(1),
                            None => 1u64,
                        };

                        // Clear any stale vote the message handler may have cast for a
                        // peer's block at this height before our production completed.
                        // Without this, add_vote's "first vote wins" rule silently
                        // drops the leader's self-vote (root cause of prepare_weight=0).
                        block_consensus_engine
                            .timevote
                            .prepare_votes
                            .remove_voter(our_addr);

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

                        // Check if prepare consensus is already reached (peer votes
                        // may have arrived and been accumulated before our self-vote).
                        // Without this, the message handler's check_prepare_consensus
                        // only triggers when a NEW peer vote arrives — if all peer
                        // votes arrived first, no further trigger occurs.
                        if block_consensus_engine
                            .timevote
                            .check_prepare_consensus(block_hash)
                        {
                            tracing::info!(
                                "✅ Prepare consensus already reached for block {} — generating precommit",
                                block_height
                            );
                            block_consensus_engine
                                .timevote
                                .generate_precommit_vote(block_hash, our_addr, our_weight);

                            // Broadcast our precommit vote
                            let precommit_sig = if let Some(signing_key) =
                                block_consensus_engine.get_signing_key()
                            {
                                use ed25519_dalek::Signer;
                                let mut msg = Vec::new();
                                msg.extend_from_slice(&block_hash);
                                msg.extend_from_slice(our_addr.as_bytes());
                                msg.extend_from_slice(b"PRECOMMIT");
                                signing_key.sign(&msg).to_bytes().to_vec()
                            } else {
                                vec![]
                            };
                            let precommit =
                                crate::network::message::NetworkMessage::TimeVotePrecommit {
                                    block_hash,
                                    voter_id: our_addr.clone(),
                                    signature: precommit_sig,
                                };
                            block_peer_registry.broadcast(precommit).await;
                        }
                    }

                    // Step 4: Wait for consensus — EVENT-DRIVEN via block_added_signal.
                    // The message handler adds the block when precommit consensus is reached,
                    // which signals block_added_signal. We await that signal with a timeout
                    // instead of polling, so consensus completes instantly when votes arrive.

                    let consensus_timeout = if blocks_behind > 0 {
                        std::time::Duration::from_secs(10) // Behind: shorter timeout
                    } else {
                        std::time::Duration::from_secs(15) // Normal: wait for consensus signal
                    };

                    let block_signal = block_blockchain.block_added_signal();

                    // Wait for either: block added (via signal) or timeout
                    let consensus_reached = tokio::time::timeout(consensus_timeout, async {
                        loop {
                            block_signal.notified().await;
                            // Check if OUR block was the one added
                            if block_blockchain.get_height() >= block_height {
                                return true;
                            }
                            // Signal was for a different block, keep waiting
                        }
                    })
                    .await;

                    match consensus_reached {
                        Ok(true) => {
                            tracing::info!("✅ Block {} finalized via consensus!", block_height);
                            block_consensus_engine
                                .timevote
                                .cleanup_block_votes(block_hash);
                        }
                        _ => {
                            // Timeout — use fallback: add block directly as leader
                            let prepare_weight = block_consensus_engine
                                .timevote
                                .get_prepare_weight(block_hash);
                            let precommit_weight = block_consensus_engine
                                .timevote
                                .get_precommit_weight(block_hash);

                            tracing::warn!(
                                "⏰ Consensus timeout for block {} after {}s (prepare={}, precommit={})",
                                block_height,
                                consensus_timeout.as_secs(),
                                prepare_weight,
                                precommit_weight
                            );

                            let validator_count =
                                block_consensus_engine.timevote.get_validators().len();
                            // Only fallback when we have SOME consensus support or the
                            // network is too small for normal voting. Never force-add a
                            // block with zero votes — that creates forks.
                            let should_fallback = prepare_weight > 0 || validator_count <= 2;

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

                            block_consensus_engine
                                .timevote
                                .cleanup_block_votes(block_hash);
                        }
                    }

                    // Check if we're still behind and need to continue immediately
                    let new_height = block_blockchain.get_height();
                    let new_expected = block_blockchain.calculate_expected_height();
                    let still_behind = new_expected.saturating_sub(new_height);
                    if still_behind > 0 {
                        tracing::info!(
                            "🔄 Still {} blocks behind expected height {}, waiting for peer sync",
                            still_behind,
                            new_expected
                        );

                        // Invalidate consensus cache so next check uses fresh peer data
                        block_blockchain.invalidate_consensus_cache().await;

                        // Request fresh chain tips from all peers
                        block_peer_registry
                            .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                            .await;

                        // Event-driven wait: wait for peers to report our new height
                        // before attempting to produce the next block
                        let tip_signal = block_peer_registry.chain_tip_updated_signal();
                        let wait_result =
                            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                                loop {
                                    tip_signal.notified().await;
                                    if block_blockchain.check_2_3_consensus_cached().await {
                                        return true;
                                    }
                                }
                            })
                            .await;

                        match wait_result {
                            Ok(true) => {
                                tracing::debug!(
                                    "✅ Peers confirmed height {} — continuing production",
                                    new_height
                                );
                            }
                            _ => {
                                tracing::debug!(
                                    "⏱️ Peer sync timeout at height {} — retrying",
                                    new_height
                                );
                            }
                        }

                        is_producing.store(false, Ordering::SeqCst);
                        interval.reset();
                        continue;
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
    // Also handles responsive behind-chain checks more frequently than 10-minute block production interval
    let status_blockchain = blockchain_server.clone();
    let status_registry = registry.clone();
    let status_production_trigger = production_trigger.clone(); // Trigger to wake up block production
    let status_ai_system = ai_system.clone();
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

                    // Check if we need rapid production (between 10-minute block production checks)
                    let expected_height = status_blockchain.calculate_expected_height();
                    let blocks_behind = expected_height.saturating_sub(height);

                    if blocks_behind > 0 {
                        let genesis_timestamp = status_blockchain.genesis_timestamp();
                        let now_timestamp = chrono::Utc::now().timestamp();
                        let expected_block_time = genesis_timestamp + (expected_height as i64 * 600);
                        let time_since_expected = now_timestamp - expected_block_time;

                        // Check if production should be triggered (>2 blocks OR >5min past)
                        let should_produce = blocks_behind > 2
                            || time_since_expected >= 300;

                        if should_produce {
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
                                    // Wake up the block production loop to produce via normal consensus
                                    tracing::debug!("⏰ Responsive sync found no peer blocks - notifying block production");
                                    status_production_trigger.notify_one();
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
                                tracing::debug!(
                                    "💾 Block Cache: {} | Memory: {}MB",
                                    cache_stats,
                                    cache_memory_mb
                                );
                            }
                        }
                    } else {
                        tracing::debug!(
                            "📊 Status: Height={}, Active Masternodes={}",
                            height,
                            mn_count
                        );

                        // Log cache statistics every 5 checks (every ~25 minutes)
                        if tick_count % 5 == 0 && tick_count > 0 {
                            let cache_stats = status_blockchain.get_cache_stats();
                            let cache_memory_mb = status_blockchain.get_cache_memory_usage() / (1024 * 1024);
                            tracing::debug!(
                                "💾 Block Cache: {} | Memory: {}MB",
                                cache_stats,
                                cache_memory_mb
                            );
                        }
                    }

                    // AI System periodic reporting (every 5 ticks / ~5 minutes)
                    if tick_count % 5 == 0 && tick_count > 0 {
                        // Collect metrics snapshot from all AI subsystems
                        status_ai_system.collect_and_record_metrics();
                        let ai_status = status_ai_system.brief_status();
                        tracing::debug!("🧠 AI System: {}", ai_status);
                    }

                    // AI attack detector cleanup (every 60 ticks / ~60 minutes)
                    if tick_count % 60 == 0 && tick_count > 0 {
                        status_ai_system.attack_detector.cleanup_old_records(
                            std::time::Duration::from_secs(3600),
                        );
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
                    "🧹 Consensus cleanup: removed {} old finalized txs. Current: {} tx_state, {} finalized",
                    removed,
                    stats.tx_state_entries,
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
        network_type,
    )
    .await
    {
        Ok(mut server) => {
            // NOTE: Masternodes announced via P2P are NOT auto-whitelisted.
            // Only peers from time-coin.io and config are trusted.

            // Wire up AI system for attack detection enforcement
            server.set_ai_system(ai_system.clone());

            // Initialize TLS for encrypted P2P connections
            let tls_config = if config.security.enable_tls {
                match crate::network::tls::TlsConfig::new_self_signed() {
                    Ok(tls) => {
                        let tls = Arc::new(tls);
                        server.set_tls_config(tls.clone());
                        tracing::info!("🔒 TLS encryption enabled for P2P connections");
                        Some(tls)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "⚠️ TLS initialization failed, running without encryption: {}",
                            e
                        );
                        None
                    }
                }
            } else {
                tracing::info!("🔓 TLS disabled by configuration");
                None
            };

            // Give registry access to network broadcast channel
            registry
                .set_broadcast_channel(server.tx_notifier.clone())
                .await;

            // Start gossip-based masternode status tracking
            registry.start_gossip_broadcaster(peer_connection_registry.clone());
            registry.start_report_cleanup();
            tracing::info!("✓ Gossip-based masternode status tracking started");

            // Share TimeLock resources with peer connection registry for outbound connections
            peer_connection_registry
                .set_timelock_resources(
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

            // Request missing blocks from peers (after network is initialized)
            if !missing_blocks.is_empty() {
                let blockchain_clone = blockchain.clone();
                let missing_clone = missing_blocks.clone();
                tokio::spawn(async move {
                    // Wait a bit for peer connections to establish
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    tracing::info!(
                        "🔄 Requesting {} missing blocks from peers",
                        missing_clone.len()
                    );
                    blockchain_clone.request_missing_blocks(missing_clone).await;
                });
            }

            // Create broadcast channel for WebSocket transaction notifications
            let (tx_event_sender, _) =
                tokio::sync::broadcast::channel::<rpc::websocket::TransactionEvent>(1000);

            // Start RPC server with access to blacklist
            let rpc_consensus = consensus_engine.clone();
            let rpc_utxo = utxo_mgr.clone();
            let rpc_registry = registry.clone();
            let rpc_blockchain = blockchain.clone();
            let rpc_addr_clone = rpc_addr.clone();
            let rpc_network = network_type;
            let rpc_shutdown_token = shutdown_token.clone();
            let rpc_blacklist = server.blacklist.clone();
            let rpc_tx_sender = tx_event_sender.clone();

            let rpc_handle = tokio::spawn(async move {
                match RpcServer::new(
                    &rpc_addr_clone,
                    rpc_consensus,
                    rpc_utxo,
                    rpc_network,
                    rpc_registry,
                    rpc_blockchain,
                    rpc_blacklist,
                    Some(rpc_tx_sender),
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

            // Start WebSocket server for real-time wallet notifications
            let ws_addr = format!("0.0.0.0:{}", network_type.default_ws_port());
            let ws_shutdown = shutdown_token.clone();
            let ws_tx_sender = tx_event_sender.clone();
            let ws_addr_display = ws_addr.clone();
            let ws_handle = tokio::spawn(async move {
                if let Err(e) =
                    rpc::websocket::start_ws_server(&ws_addr, ws_tx_sender, ws_shutdown).await
                {
                    eprintln!("  ❌ WebSocket server error: {}", e);
                }
            });
            shutdown_manager.register_task(ws_handle);

            // Now create network client for outbound connections
            let mut network_client = network::client::NetworkClient::new(
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
                Some(server.blacklist.clone()),
            );
            // Share AISystem's reconnection AI so connection learning data is unified
            network_client.set_reconnection_ai(ai_system.reconnection_ai.clone());
            if let Some(ref tls) = tls_config {
                network_client.set_tls_config(tls.clone());
            }
            network_client.start().await;

            // BOOTSTRAP: At genesis, aggressively request masternode lists from all peers
            // This ensures nodes discover each other for block production
            let bootstrap_registry = registry.clone();
            let bootstrap_peer_registry = peer_connection_registry.clone();
            let bootstrap_blockchain = blockchain.clone();
            let bootstrap_shutdown = shutdown_token.clone();
            tokio::spawn(async move {
                // At height 0, periodically request masternodes every 5 seconds
                // until we have at least 3 active masternodes or height advances
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));

                loop {
                    tokio::select! {
                        _ = bootstrap_shutdown.cancelled() => {
                            break;
                        }
                        _ = interval.tick() => {
                            let current_height = bootstrap_blockchain.get_height();
                            if current_height > 0 {
                                // No longer at genesis, stop bootstrap discovery
                                tracing::info!("✓ Bootstrap complete: Height advanced to {}", current_height);
                                break;
                            }

                            let active_count = bootstrap_registry.count_active().await;
                            if active_count >= 3 {
                                tracing::debug!("✓ Bootstrap satisfied: {} active masternodes", active_count);
                                continue; // Keep checking in case we drop below 3
                            }

                            // Still need more masternodes - request from all peers
                            let connected_peers = bootstrap_peer_registry.get_connected_peers().await;
                            if !connected_peers.is_empty() {
                                tracing::info!(
                                    "🌱 Bootstrap discovery: {} active/{} registered, requesting from {} peers",
                                    active_count,
                                    bootstrap_registry.count().await,
                                    connected_peers.len()
                                );

                                for peer_ip in &connected_peers {
                                    let msg = crate::network::message::NetworkMessage::GetMasternodes;
                                    let _ = bootstrap_peer_registry.send_to_peer(peer_ip, msg).await;
                                }
                            } else {
                                tracing::warn!("⚠️ Bootstrap discovery: No connected peers found");
                            }
                        }
                    }
                }
            });

            println!("\n╔═══════════════════════════════════════════════════════╗");
            println!("║  🎉 TIME Coin Daemon is Running!                      ║");
            println!("╠═══════════════════════════════════════════════════════╣");
            println!("║  Network:    {:<40} ║", format!("{:?}", network_type));
            println!("║  Storage:    {:<40} ║", config.storage.backend);
            println!("║  P2P Port:   {:<40} ║", p2p_addr);
            println!("║  RPC Port:   {:<40} ║", rpc_addr);
            println!("║  WS Port:    {:<40} ║", ws_addr_display);
            println!("║  Consensus:  TimeLock + TimeVote Hybrid               ║");
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

            // CRITICAL: Flush sled databases to disk before exit
            // Without this, in-memory dirty pages are lost on process termination,
            // causing block corruption ("unexpected end of file") on restart.
            tracing::info!("💾 Flushing block storage to disk...");
            if let Err(e) = block_storage_for_shutdown.flush() {
                tracing::error!("Failed to flush block storage on shutdown: {}", e);
            } else {
                tracing::info!("✓ Block storage flushed successfully");
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
