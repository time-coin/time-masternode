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
pub mod governance;
pub mod http_client;
pub mod masternode_authority;
pub mod masternode_certificate;
pub mod masternode_registry;
pub mod memo;
pub mod network;
pub mod network_type;
pub mod peer_manager;
pub mod purge_list;
pub mod reward_calculator;
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

    /// Force a full UTXO + transaction reindex before starting the network.
    /// The update script can also trigger this automatically by creating the
    /// file <data-dir>/reindex_requested before restarting the daemon.
    #[arg(long)]
    reindex: bool,
}

// Ensure at least 4 worker threads regardless of CPU count.
// On 1-CPU VPS machines the default (num_cpus) gives only 1 worker,
// which means any synchronous sled I/O during block sync starves
// the RPC server, timers, and network I/O.
#[tokio::main(worker_threads = 4)]
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
        match config::generate_default_masternode_conf(&gen_mn, None) {
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
                    if let Err(e) = config::generate_default_masternode_conf(&new_mn, None) {
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
                        if let Some(addrs) = conf_values.get("reward_address") {
                            if let Some(addr) = addrs.last() {
                                cfg.masternode.reward_address = addr.clone();
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

    let _log_guard = setup_logging(
        &config.logging,
        args.verbose,
        &config::get_network_data_dir(&config.node.network_type()),
    );

    // Visual separator in log files so daemon restarts are easy to spot
    tracing::info!("");
    tracing::info!("════════════════════════════════════════════════════════════");
    tracing::info!("  🚀 TIME COIN DAEMON STARTING");
    tracing::info!("════════════════════════════════════════════════════════════");
    tracing::info!("");

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

    // Detect startup-reindex triggers:
    //   1. --reindex CLI flag — operator wants a manual forced reindex.
    //   2. Sentinel file <data-dir>/reindex_requested — created by update.sh
    //      before restarting the daemon so the node automatically reindexes
    //      after a software update without any user interaction.
    let sentinel_file = std::path::Path::new(&config.storage.data_dir).join("reindex_requested");
    let reindex_on_startup = args.reindex || sentinel_file.exists();
    if reindex_on_startup {
        let reason = if args.reindex {
            "--reindex flag"
        } else {
            "reindex_requested sentinel file"
        };
        tracing::info!(
            "🔄 [STARTUP] Full reindex requested via {} — will run before network starts",
            reason
        );
        println!("🔄 Startup reindex requested ({reason}) — rebuilding UTXO set from genesis");
    }

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

    // Decode masternodeprivkey from time.conf if provided (used as consensus signing key)
    // If not set, auto-generate one and append it to time.conf
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
        } else if config.masternode.enabled {
            // Auto-generate a masternodeprivkey and persist it to time.conf
            let mut seed = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut seed);
            let encoded = masternode_certificate::encode_masternode_key(&seed);
            let key = ed25519_dalek::SigningKey::from_bytes(&seed);
            println!("✓ Auto-generated masternodeprivkey");

            // Append to time.conf so it persists across restarts
            if let Err(e) = config::append_conf_key(&conf_path, "masternodeprivkey", &encoded) {
                eprintln!("⚠️ Could not save masternodeprivkey to time.conf: {}", e);
            } else {
                println!("  └─ Saved to {}", conf_path.display());
            }
            config.masternode.masternodeprivkey = encoded;
            Some(key)
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
        // Use reward_address from config if set, otherwise fall back to auto-generated wallet address
        let wallet_address = if !config.masternode.reward_address.is_empty() {
            // Validate the reward address
            match crate::address::Address::from_string(&config.masternode.reward_address) {
                Ok(addr) => {
                    if addr.network() != network_type {
                        let expected = match network_type {
                            NetworkType::Testnet => "TIME0",
                            NetworkType::Mainnet => "TIME1",
                        };
                        let got = match addr.network() {
                            NetworkType::Testnet => "testnet (TIME0...)",
                            NetworkType::Mainnet => "mainnet (TIME1...)",
                        };
                        eprintln!(
                            "⚠️ reward_address is a {} address, but this node is running on {}.",
                            got,
                            if network_type == NetworkType::Testnet {
                                "testnet"
                            } else {
                                "mainnet"
                            }
                        );
                        eprintln!(
                            "   Expected an address starting with {}. Falling back to local wallet address.",
                            expected
                        );
                        wallet.address().to_string()
                    } else {
                        println!(
                            "✓ Using reward address from time.conf: {}",
                            config.masternode.reward_address
                        );
                        config.masternode.reward_address.clone()
                    }
                }
                Err(e) => {
                    eprintln!(
                        "⚠️ Invalid reward_address in time.conf: {} — falling back to local wallet address.",
                        e
                    );
                    wallet.address().to_string()
                }
            }
        } else {
            wallet.address().to_string()
        };

        // Get external address and extract IP only (no port) for consistent masternode identification
        let full_address = config.network.full_external_address(&network_type);
        let ip_only = full_address
            .split(':')
            .next()
            .unwrap_or(&full_address)
            .to_string();

        // Parse collateral outpoint if provided (for staked tiers)
        let has_collateral = !config.masternode.collateral_txid.is_empty()
            && config.masternode.collateral_txid != "0".repeat(64);

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
            if let Err(e) = std::fs::create_dir_all(&db_dir) {
                println!("  ⚠ Failed to create db directory: {}", e);
            }
            match storage::SledUtxoStorage::new(&db_dir) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    println!("  ⚠ Sled failed: {}", e);
                    // Attempt disk-space recovery before falling back to in-memory
                    let is_recoverable = matches!(e, storage::StorageError::Database(ref de)
                        if matches!(de, sled::Error::Corruption { .. })
                            || matches!(de, sled::Error::Io(_))
                            || de.to_string().contains("corrupted")
                            || de.to_string().contains("No space left"));
                    if is_recoverable {
                        println!("  └─ Freeing disk space and wiping corrupted UTXO db...");
                        // free_disk_space and remove_dir_all are defined later in this function;
                        // inline the same logic here since nested fns aren't closures.
                        let log1 = format!("{}/debug.log.1", config.storage.data_dir);
                        if let Ok(meta) = std::fs::metadata(&log1) {
                            if std::fs::remove_file(&log1).is_ok() {
                                println!("  └─ Freed rotated log ({} MB)", meta.len() / (1024 * 1024));
                            }
                        }
                        let _ = std::fs::remove_dir_all(&db_dir);
                        let _ = std::fs::create_dir_all(&db_dir);
                    }
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
        let available_memory = get_available_memory();

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

    /// Get available system memory without the sysinfo crate.
    fn get_available_memory() -> u64 {
        // Linux: parse /proc/meminfo
        #[cfg(target_os = "linux")]
        {
            if let Ok(contents) = std::fs::read_to_string("/proc/meminfo") {
                for line in contents.lines() {
                    if line.starts_with("MemAvailable:") {
                        if let Some(kb) = line.split_whitespace().nth(1) {
                            if let Ok(val) = kb.parse::<u64>() {
                                return val * 1024; // kB → bytes
                            }
                        }
                    }
                }
            }
        }
        // Windows: use GlobalMemoryStatusEx via win32 API
        #[cfg(target_os = "windows")]
        {
            use std::mem::{size_of, zeroed};
            #[repr(C)]
            struct MemoryStatusEx {
                dw_length: u32,
                dw_memory_load: u32,
                ull_total_phys: u64,
                ull_avail_phys: u64,
                ull_total_page_file: u64,
                ull_avail_page_file: u64,
                ull_total_virtual: u64,
                ull_avail_virtual: u64,
                ull_avail_extended_virtual: u64,
            }
            extern "system" {
                fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
            }
            unsafe {
                let mut status: MemoryStatusEx = zeroed();
                status.dw_length = size_of::<MemoryStatusEx>() as u32;
                if GlobalMemoryStatusEx(&mut status) != 0 {
                    return status.ull_avail_phys;
                }
            }
        }
        // macOS: use sysctl
        #[cfg(target_os = "macos")]
        {
            use std::mem::{size_of, zeroed};
            extern "C" {
                fn sysctlbyname(
                    name: *const u8,
                    oldp: *mut std::ffi::c_void,
                    oldlenp: *mut usize,
                    newp: *const std::ffi::c_void,
                    newlen: usize,
                ) -> i32;
            }
            unsafe {
                let mut mem: u64 = zeroed();
                let mut len = size_of::<u64>();
                if sysctlbyname(
                    b"hw.memsize\0".as_ptr(),
                    &mut mem as *mut u64 as *mut std::ffi::c_void,
                    &mut len,
                    std::ptr::null(),
                    0,
                ) == 0
                {
                    return mem;
                }
            }
        }
        // Fallback: assume 1 GB
        1024 * 1024 * 1024
    }

    let cache_size = calculate_cache_size();

    /// Return available bytes on the filesystem containing `path`, or None if unavailable.
    fn available_disk_bytes(_path: &str) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            use std::ffi::CString;
            #[repr(C)]
            #[allow(non_camel_case_types)]
            struct statvfs {
                f_bsize: u64, f_frsize: u64, f_blocks: u64, f_bfree: u64,
                f_bavail: u64, f_files: u64, f_ffree: u64, f_favail: u64,
                f_fsid: u64, f_flag: u64, f_namemax: u64, _pad: [u64; 6],
            }
            extern "C" {
                fn statvfs(path: *const i8, buf: *mut statvfs) -> i32;
            }
            if let Ok(cpath) = CString::new(_path) {
                unsafe {
                    let mut st: statvfs = std::mem::zeroed();
                    if statvfs(cpath.as_ptr(), &mut st) == 0 {
                        return Some(st.f_frsize * st.f_bavail);
                    }
                }
            }
        }
        None
    }

    /// Free recoverable disk space under `data_dir`:
    /// - Deletes the rotated log (debug.log.1) if present.
    /// - Removes any leftover stale corrupted-db backup directories.
    /// Returns approximate bytes freed.
    fn free_disk_space(data_dir: &str) -> u64 {
        let mut freed: u64 = 0;

        // Remove rotated debug log (debug.log.1 is a safe discard)
        let log1 = format!("{}/debug.log.1", data_dir);
        if let Ok(meta) = std::fs::metadata(&log1) {
            let sz = meta.len();
            if std::fs::remove_file(&log1).is_ok() {
                eprintln!("  └─ Freed rotated log ({} MB)", sz / (1024 * 1024));
                freed += sz;
            }
        }

        // Remove stale .corrupted.* directories left by previous recovery attempts
        if let Ok(entries) = std::fs::read_dir(data_dir) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().contains(".corrupted.") {
                    let sz: u64 = walkdir_size(&entry.path());
                    if std::fs::remove_dir_all(entry.path()).is_ok() {
                        eprintln!(
                            "  └─ Removed stale backup '{}' ({} MB)",
                            entry.file_name().to_string_lossy(),
                            sz / (1024 * 1024)
                        );
                        freed += sz;
                    }
                }
            }
        }

        freed
    }

    /// Approximate directory size by summing file lengths (best-effort).
    fn walkdir_size(dir: &std::path::Path) -> u64 {
        let mut total = 0u64;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    total += walkdir_size(&path);
                } else if let Ok(m) = std::fs::metadata(&path) {
                    total += m.len();
                }
            }
        }
        total
    }

    /// Attempt to open a sled database. On corruption or I/O failure:
    ///  1. Free recoverable disk space (rotated logs, stale backups).
    ///  2. Delete the corrupted database directory — no backup copy is made
    ///     because backing up on a near-full disk would worsen the problem.
    ///  3. Retry once with a fresh, empty database (chain re-syncs from peers).
    fn open_sled_with_recovery(
        path: &str,
        cache_size: u64,
        data_dir: &str,
    ) -> Result<sled::Db, sled::Error> {
        let try_open = |p: &str| {
            sled::Config::new()
                .path(p)
                .cache_capacity(cache_size)
                .flush_every_ms(None)
                .mode(sled::Mode::LowSpace)
                .open()
        };

        match try_open(path) {
            Ok(db) => Ok(db),
            Err(e) => {
                let is_recoverable = matches!(e, sled::Error::Corruption { .. })
                    || matches!(&e, sled::Error::Io(_))
                    || e.to_string().contains("corrupted")
                    || e.to_string().contains("No space left");

                if !is_recoverable {
                    return Err(e);
                }

                eprintln!("⚠️  Sled failed at '{}': {}", path, e);

                // Report available disk space before cleanup
                if let Some(avail) = available_disk_bytes(data_dir) {
                    eprintln!(
                        "  └─ Available disk space: {} MB",
                        avail / (1024 * 1024)
                    );
                }

                // Free recoverable space first, then wipe the broken database
                let freed = free_disk_space(data_dir);
                if freed > 0 {
                    eprintln!("  └─ Total freed: {} MB", freed / (1024 * 1024));
                }

                eprintln!("  └─ Removing corrupted database directory...");
                if let Err(re) = std::fs::remove_dir_all(path) {
                    eprintln!("  ⚠ Could not remove '{}': {}", path, re);
                }

                if let Some(avail) = available_disk_bytes(data_dir) {
                    eprintln!(
                        "  └─ Available disk space after cleanup: {} MB",
                        avail / (1024 * 1024)
                    );
                    if avail < 200 * 1024 * 1024 {
                        eprintln!("  ⚠ WARNING: Less than 200 MB free — node may fail again soon.");
                        eprintln!("    Free disk space on the host before restarting.");
                    }
                }

                eprintln!("  └─ Starting with a fresh database — chain will re-sync from peers");
                try_open(path)
            }
        }
    }

    // Initialize block storage
    let db_dir = format!("{}/db", config.storage.data_dir);

    // Warn early if disk space is critically low
    if let Some(avail) = available_disk_bytes(&config.storage.data_dir) {
        let avail_mb = avail / (1024 * 1024);
        if avail_mb < 500 {
            eprintln!(
                "⚠️  WARNING: Only {} MB of disk space available on the data directory filesystem.",
                avail_mb
            );
            eprintln!("   Running low on disk space can corrupt the database.");
            eprintln!("   Free disk space before problems occur.");
        } else {
            println!("  └─ Disk space available: {} MB", avail_mb);
        }
    }

    let block_storage_path = format!("{}/blocks", db_dir);
    let block_storage =
        match open_sled_with_recovery(&block_storage_path, cache_size, &config.storage.data_dir) {
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

    // Enable persistent spent-UTXO tombstone storage (must be before initialize_states)
    if let Err(e) = utxo_mgr.enable_spent_persistence(&block_storage) {
        tracing::warn!(
            "⚠️ Failed to enable spent UTXO tombstone persistence: {:?}",
            e
        );
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

    // Release stale local collateral if the config changed (e.g., user commented
    // out their collateral line and restarted).  Compare the previously saved
    // outpoint with the current config so the UTXO becomes spendable again.
    //
    // stale_local_outpoints is kept alive so rebuild_collateral_locks (below) can
    // exclude these outpoints — otherwise the old registry entry re-locks them.
    let stale_local_outpoints: std::collections::HashSet<crate::types::OutPoint> = {
        let current_local_outpoint = masternode_info
            .as_ref()
            .filter(|mn| mn.tier != types::MasternodeTier::Free)
            .and_then(|mn| mn.collateral_outpoint.clone());

        let mut stale = std::collections::HashSet::new();

        // Primary path: compare saved outpoint vs current config.
        if let Some(prev) = utxo_mgr.load_local_collateral_outpoint() {
            let should_release = match &current_local_outpoint {
                Some(cur) => cur != &prev, // collateral changed
                None => true,              // collateral removed
            };
            if should_release {
                utxo_mgr.release_stale_local_collateral(&prev);
                stale.insert(prev);
            }
        }

        // Fallback: if __local_collateral_outpoint__ was never saved (first run,
        // sled was wiped, or daemon was killed before the save), the branch above
        // is a no-op and any lock that was persisted by a previous run will linger.
        // Walk every loaded collateral lock and release any that don't match the
        // current config so they don't show as "locked" in the dashboard.
        let all_locked = utxo_mgr.list_locked_collaterals();
        for lc in all_locked {
            let matches_current = current_local_outpoint
                .as_ref()
                .map(|cur| cur == &lc.outpoint)
                .unwrap_or(false);
            if !matches_current {
                utxo_mgr.release_stale_local_collateral(&lc.outpoint);
                stale.insert(lc.outpoint);
            }
        }

        // Persist the current config for next restart
        utxo_mgr.save_local_collateral_outpoint(current_local_outpoint.as_ref());

        stale
    };

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
                    eprintln!("   The collateral transaction may not be confirmed yet.");
                    eprintln!("   Node will start as Free tier and automatically upgrade");
                    eprintln!(
                        "   to the correct tier once the on-chain registration is processed."
                    );
                    eprintln!("   Tip: Set tier=bronze (or silver/gold) in time.conf to skip auto-detection.");
                    println!("✓ Running as Free masternode (provisional — will upgrade on-chain)");
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
    registry.set_utxo_manager(utxo_mgr.clone()).await;
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

    // Enable write-through mempool persistence so transactions survive hard kills.
    // Must be called before load_mempool_from_sled so the tree handle is ready.
    consensus_engine.enable_mempool_persistence(&block_storage);

    // Restore mempool from the previous run so finalized and pending transactions
    // survive daemon restarts. This runs before consensus is fully wired up, so
    // pending entries are placed in the pool without triggering new TimeVote rounds.
    let restored = consensus_engine.load_mempool_from_sled(&block_storage);
    if restored > 0 {
        tracing::info!("📂 Restored {} mempool transaction(s) from disk", restored);
    }

    // AV41: purge any ghost TXs (0 inputs, 0 outputs, invalid/no special_data)
    // that persisted in the sled mempool from before this fix was deployed.
    let ghost_purged = consensus_engine.tx_pool.purge_ghost_transactions();
    if ghost_purged > 0 {
        tracing::warn!(
            "🛡️ [AV41] Startup: purged {} ghost transaction(s) from restored mempool",
            ghost_purged
        );
    }

    // Apply the hardcoded phantom-finalized blacklist (see src/purge_list.rs)
    // and then sweep any other finalized TXs whose inputs are missing on-chain.
    let blacklist_purged = consensus_engine.purge_blacklisted_transactions().await;
    if blacklist_purged > 0 {
        tracing::warn!(
            "🛡️ Startup: applied phantom-TX blacklist to {} record(s)",
            blacklist_purged
        );
    }
    let stuck_evicted = consensus_engine.evict_finalized_with_missing_inputs().await;
    if stuck_evicted > 0 {
        tracing::warn!(
            "🛡️ Startup: evicted {} finalized TX(s) with missing input UTXOs",
            stuck_evicted
        );
    }

    let consensus_engine = Arc::new(consensus_engine);
    tracing::info!("✓ Consensus engine initialized with AI validation and TimeLock voting");

    // Keep a reference for persisting the mempool on clean shutdown
    let consensus_for_shutdown = consensus_engine.clone();

    // Initialize blockchain (clone sled handle so governance can share the same DB)
    let gov_storage = block_storage.clone();
    // Keep a clone for blacklist persistence (wired after network server starts)
    let blacklist_storage = block_storage.clone();
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

    // Initialize on-chain governance subsystem
    match governance::GovernanceState::new(gov_storage) {
        Ok(gov) => {
            blockchain.set_governance(Arc::new(gov));
            tracing::info!("🏛️  Governance subsystem initialized");
        }
        Err(e) => tracing::warn!("🏛️  Governance init failed (non-fatal): {e}"),
    }

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

    // One-shot sweep for block-construction artifacts (coinbase +
    // reward_distribution) that leaked into the finalized pool from pre-fix
    // reorgs or from peers running older code. Definitive, chain-aware —
    // reward_distribution detection requires tx_index which is now wired.
    let block_only_purged = blockchain.purge_block_only_finalized();
    if block_only_purged > 0 {
        tracing::warn!(
            "🧹 Startup: removed {} block-only TX(s) from finalized pool",
            block_only_purged
        );
    }

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

    // ── PRE-LAUNCH HEIGHT GUARD ───────────────────────────────────────────────
    // If our stored chain height is higher than what could have been produced
    // since the genesis timestamp, we have stale blocks from before launch
    // (e.g. a test run against an old genesis timestamp).  Roll back to genesis
    // automatically so the node resyncs the correct chain from peers.
    {
        let genesis_ts = blockchain.genesis_timestamp();
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let max_valid_height = if now_secs >= genesis_ts {
            ((now_secs - genesis_ts) / 600) as u64
        } else {
            0
        };
        let stored_height = blockchain.get_height();
        if stored_height > max_valid_height {
            tracing::warn!(
                "⚠️  Stored chain height {} exceeds max valid height {} (genesis launch: {}). \
                 Chain contains pre-launch blocks — rolling back to genesis and resyncing.",
                stored_height,
                max_valid_height,
                chrono::DateTime::from_timestamp(genesis_ts, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                    .unwrap_or_else(|| genesis_ts.to_string()),
            );
            eprintln!(
                "⚠️  Rolling back stale chain (height {}) to genesis — stored blocks predate launch time.",
                stored_height
            );
            blockchain.revert_to_after_genesis().await;
            tracing::info!(
                "✅ Rollback complete — chain is now at height 0, will resync from peers."
            );
        }
    }
    // ── END PRE-LAUNCH HEIGHT GUARD ───────────────────────────────────────────

    // ── PRE-GENESIS BLOCK SCAN ────────────────────────────────────────────────
    // Scan sled directly for any block (height ≥ 1) whose timestamp predates
    // genesis.  Uses scan_prefix("block_") so every stored block is checked
    // regardless of deserialization quirks or block_cache state.
    {
        let purged = blockchain.purge_pre_genesis_blocks().await;
        if purged > 0 {
            tracing::info!(
                "✅ Startup purge complete — removed {} pre-genesis block(s), chain reset to height 0.",
                purged
            );
            eprintln!(
                "⚠️  Removed {} pre-genesis block(s) from sled — chain reset to height 0, will resync.",
                purged
            );
        }
    }
    // ── WRONG-CHAIN BLOCK SCAN ───────────────────────────────────────────────
    // If block 1's previous_hash doesn't match our genesis, we have stale data
    // from an incompatible chain (e.g. old April-1 chain).  Purge and resync.
    {
        let purged = blockchain.purge_wrong_chain_blocks().await;
        if purged > 0 {
            tracing::warn!(
                "✅ Wrong-chain purge complete — removed {} block(s) referencing a different genesis, \
                 chain reset to height 0. Will resync from compatible peers.",
                purged
            );
            eprintln!(
                "⚠️  Removed {} wrong-chain block(s) from sled — chain reset to height 0, will resync.",
                purged
            );
        }
    }
    // ── END WRONG-CHAIN BLOCK SCAN ───────────────────────────────────────────

    // Build (or rebuild) transaction index on startup.
    // build_tx_index() clears any stale entries before rebuilding from scratch, so it is safe
    // to always run it. A stale index (e.g., from an incomplete rollback) causes
    // validate_block_rewards to look up the wrong transaction and reject valid peer blocks.
    if let Some(ref _idx) = tx_index {
        if blockchain.get_height() > 0 || reindex_on_startup {
            tracing::info!("📊 Building transaction index from blockchain...");
            if let Err(e) = blockchain.build_tx_index().await {
                tracing::warn!("Failed to build transaction index: {}", e);
            }
        }
    }

    // Rebuild in-memory UTXO set from stored blocks on every startup.
    //
    // The UTXO set (InMemoryUtxoStorage) is entirely in memory — it does not survive
    // daemon restarts.  Without this reindex, the UTXO set is empty after restart, so
    // every paid-tier masternode announcement fails collateral verification ("collateral
    // UTXO not found on-chain") even though the UTXO exists on-chain.  This prevents
    // any paid-tier node from being counted as active, breaking consensus quorum.
    //
    // Must run before rebuild_collateral_locks so purge_stale_collateral_locks can
    // validate locks against real UTXO states rather than an empty set.
    //
    // reindex_on_startup forces this even at height 0 (e.g., first-run after update).
    if blockchain.get_height() > 0 || reindex_on_startup {
        let h = blockchain.get_height();
        if reindex_on_startup {
            tracing::info!(
                "🔄 [STARTUP] Forced full reindex: rebuilding UTXO set from {} stored block(s)...",
                h
            );
        } else {
            tracing::info!(
                "🔄 [STARTUP] Rebuilding in-memory UTXO set from {} stored block(s)...",
                h
            );
        }
        match blockchain.reindex_utxos().await {
            Ok((blocks, utxos)) => {
                tracing::info!(
                    "✅ [STARTUP] UTXO reindex complete: {} blocks replayed, {} unspent UTXOs",
                    blocks,
                    utxos
                );
                // Delete the sentinel file now that reindex succeeded, so the
                // next restart is normal (no unnecessary rebuild).
                if sentinel_file.exists() {
                    if let Err(e) = std::fs::remove_file(&sentinel_file) {
                        tracing::warn!(
                            "⚠️ Failed to remove reindex sentinel file {:?}: {}",
                            sentinel_file,
                            e
                        );
                    } else {
                        tracing::info!(
                            "✅ [STARTUP] Reindex sentinel file removed — next restart will be normal"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "⚠️ [STARTUP] UTXO reindex failed: {} — collateral verification may reject valid masternodes",
                    e
                );
                // Keep the sentinel file so the next restart retries.
            }
        }
    }

    println!("✓ Blockchain initialized");
    println!();

    // Reconstruct the blocks_without_reward counter for every masternode from the
    // persisted last_reward_height.  This avoids a full 1000-block sled scan at
    // startup and ensures get_reward_tracking_from_memory() is accurate from block 1.
    {
        let chain_height = blockchain.get_height();
        registry.reconstruct_reward_counters(chain_height).await;
        tracing::info!(
            "📊 Reconstructed reward counters for masternodes at height {}",
            chain_height
        );
    }

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
    let missing_blocks = blockchain.verify_chain_integrity().await;
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

    // Scan for and repair any UTXO outputs that are confirmed on-chain but missing
    // from the local sled store.  Runs synchronously before the network starts so
    // that the tier auto-detection and wallet balance are correct on this boot.
    blockchain.scan_and_repair_utxo_gaps().await;

    // Create shared peer connection registry for both client and server
    let peer_connection_registry = Arc::new(PeerConnectionRegistry::new());

    // Shared timestamp updated whenever a block is added; used by PartitionDetector.
    let last_block_time = Arc::new(std::sync::atomic::AtomicU64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    ));

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
        connection_manager.set_local_ip(ip.clone());
    }

    // Network client will be started after server is created so we can share resources

    // Create sync completion notifier for masternode announcement
    let sync_complete = Arc::new(tokio::sync::Notify::new());

    // Register this node if running as masternode
    let masternode_address = masternode_info.as_ref().map(|mn| mn.address.clone());

    if let Some(mut mn) = masternode_info {
        // If tier auto-detection failed at startup (UTXO not in local storage), try to
        // recover the correct tier from the registry DB that was loaded from disk.
        // This handles collateral UTXOs that are spent/archived in the UTXO set but
        // were previously anchored via an on-chain MasternodeReg transaction.
        if mn.tier == types::MasternodeTier::Free {
            if let Some(ref outpoint) = mn.collateral_outpoint.clone() {
                if let Some(existing) = registry.get(&mn.address).await {
                    let existing_tier = existing.masternode.tier;
                    if existing_tier != types::MasternodeTier::Free
                        && existing.masternode.collateral_outpoint.as_ref() == Some(outpoint)
                    {
                        tracing::info!(
                            "✓ Recovered {:?} tier from on-disk registry for {} \
                             (collateral UTXO unavailable at startup — using persisted registration)",
                            existing_tier,
                            mn.address
                        );
                        mn.tier = existing_tier;
                        mn.collateral = existing_tier.collateral();
                    }
                }
            }
        }

        // When tier auto-detection deferred (UTXO not in local storage, chain not yet
        // synced), mn.tier is provisionally Free but still carries the collateral outpoint.
        // AV40 blocks Free+outpoint registrations to prevent registry poisoning. Strip the
        // outpoint from the local registry entry — mn retains it for mn_for_sync, which
        // handles on-chain paid-tier registration once the chain is accessible.
        let mn_to_register =
            if mn.tier == types::MasternodeTier::Free && mn.collateral_outpoint.is_some() {
                let mut provisional = mn.clone();
                provisional.collateral_outpoint = None;
                provisional
            } else {
                mn.clone()
            };

        // Pre-declare as local so AV40's is_local_update exemption fires correctly
        // for the self-registration path (e.g. provisional Free tier while new
        // collateral is unconfirmed — without this the daemon exits on startup).
        registry.set_local_masternode(mn.address.clone()).await;

        // Try registration; on squatter conflict, evict and retry once.
        let registration_ok = match registry
            .register(
                mn_to_register.clone(),
                mn_to_register.wallet_address.clone(),
            )
            .await
        {
            Ok(()) => true,
            Err(crate::masternode_registry::RegistryError::CollateralAlreadyLocked) => {
                // A gossip squatter holds our collateral in the local registry.
                // Evict the squatter so we show up correctly in the local registry
                // and dashboard, then broadcast a V4 proof to claim it on peers.
                if let Some(ref outpoint) = mn.collateral_outpoint {
                    if let Some(sq) = registry.find_holder_of_outpoint(outpoint).await {
                        tracing::warn!(
                            "🛡️ Evicting gossip squatter {} from local registry (collateral belongs to us)",
                            sq
                        );
                        let _ = registry.unregister(&sq).await;
                    }
                }
                // Clear any stale self-entry (e.g. from a previous collateral migration).
                // Safe: we're about to immediately re-register with the current outpoint.
                let _ = registry.unregister(&mn.address).await;

                match registry
                    .register(
                        mn_to_register.clone(),
                        mn_to_register.wallet_address.clone(),
                    )
                    .await
                {
                    Ok(_) => {
                        tracing::info!("✅ Local masternode re-registered after evicting squatter");
                        true
                    }
                    Err(e2) => {
                        tracing::warn!(
                            "⚠️ Could not re-register after squatter eviction ({:?}); \
                             will retry via V4 broadcast",
                            e2
                        );
                        // Still mark as local so the V4 broadcast can claim it on peers.
                        registry.set_local_masternode(mn.address.clone()).await;
                        false
                    }
                }
            }
            Err(e) => {
                tracing::error!("❌ Failed to register masternode: {}", e);
                std::process::exit(1);
            }
        };

        if registration_ok {
            // Local masternode must be OnChain so it persists across
            // peer disconnects (Handshake nodes are removed on disconnect).
            if mn.tier != types::MasternodeTier::Free {
                let _ = registry
                    .set_registration_source(
                        &mn.address,
                        crate::masternode_registry::RegistrationSource::OnChain(
                            blockchain.get_height(),
                        ),
                    )
                    .await;
            }

            // Store empty certificate in registry (certificate system removed)
            registry.set_local_certificate([0u8; 64]).await;

            // Set signing key: use masternodeprivkey from time.conf if provided,
            // otherwise fall back to the wallet's auto-generated key.
            let signing_key = if let Some(ref mn_key) = masternode_signing_key {
                tracing::info!("✓ Using masternodeprivkey for consensus signing");
                mn_key.clone()
            } else {
                tracing::info!(
                    "✓ Using wallet key for consensus signing (no masternodeprivkey in time.conf)"
                );
                wallet.signing_key().clone()
            };
            if let Err(e) = consensus_engine.set_identity(mn.address.clone(), signing_key) {
                eprintln!("⚠️ Failed to set consensus identity: {}", e);
            }
            // Transaction inputs must be signed with the wallet key so the derived
            // address matches the UTXOs' script_pubkey.
            if let Err(e) = consensus_engine.set_wallet_signing_key(wallet.signing_key().clone()) {
                eprintln!("⚠️ Failed to set wallet signing key: {}", e);
            }
            // Pre-seed the pubkey cache so self-sends can encrypt memos immediately.
            consensus_engine.utxo_manager.register_pubkey(
                &mn.wallet_address,
                wallet.signing_key().verifying_key().to_bytes(),
            );

            tracing::info!("✓ Registered masternode: {}", mn.wallet_address);
            tracing::info!("✓ Consensus engine identity configured with wallet key");

            // Lock collateral UTXO so it shows as locked in wallet balance
            if mn.tier != types::MasternodeTier::Free {
                if let Some(ref outpoint) = mn.collateral_outpoint {
                    let lock_height = blockchain.get_height();
                    if let Err(e) = consensus_engine.utxo_manager.lock_local_collateral(
                        outpoint.clone(),
                        mn.address.clone(),
                        lock_height,
                        mn.tier.collateral(),
                    ) {
                        tracing::warn!("⚠️ Failed to lock local collateral UTXO: {:?}", e);
                    } else {
                        tracing::info!("🔒 Locked collateral UTXO for {:?} tier", mn.tier);
                    }
                }
            }

            // Rebuild collateral locks for all OTHER masternodes (not local — already locked above).
            // Don't re-lock outpoints we just released as stale.
            {
                let all_masternodes = registry.list_all().await;
                let lock_height = blockchain.get_height();
                let entries: Vec<_> = all_masternodes
                    .iter()
                    .filter(|info| info.masternode.address != mn.address)
                    .filter(|info| {
                        !info
                            .masternode
                            .collateral_outpoint
                            .as_ref()
                            .map(|op| stale_local_outpoints.contains(op))
                            .unwrap_or(false)
                    })
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
        } // end if registration_ok

        // Start peer exchange task (for masternode discovery)
        let peer_connection_registry_clone = peer_connection_registry.clone();
        let shutdown_token_clone = shutdown_token.clone();
        let peer_exchange_blockchain = blockchain.clone();
        let peer_exchange_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = shutdown_token_clone.cancelled() => {
                        tracing::debug!("🛑 Peer exchange task shutting down gracefully");
                        break;
                    }
                    _ = interval.tick() => {
                        // Skip during sync — focus on catching up
                        if peer_exchange_blockchain.is_syncing() {
                            continue;
                        }
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
        let health_blockchain = blockchain.clone();
        let health_handle = tokio::spawn(async move {
            // Wait for peers to connect before first health check
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60)); // Every minute
            loop {
                tokio::select! {
                    _ = health_shutdown.cancelled() => {
                        tracing::debug!("🛑 Health monitoring task shutting down gracefully");
                        break;
                    }
                    _ = interval.tick() => {
                        // Skip during sync — focus on catching up
                        if health_blockchain.is_syncing() {
                            continue;
                        }
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

                        // Remove Free-tier nodes that have been offline for >300 s.
                        // They are kept in the registry after disconnect for a grace
                        // window; this call prunes the ones that never came back.
                        health_registry.clean_stale_free_tier_nodes(300).await;

                        // If unhealthy, ensure inactive masternodes are in the peer
                        // discovery list. Actual reconnection is handled by Phase 3-MN
                        // in NetworkClient (runs every 30s, iterates all registered
                        // masternodes and spawns connections to unconnected ones).
                        if health.active_masternodes < 5 {
                            let inactive_addresses = health_registry.get_inactive_masternode_addresses().await;
                            if !inactive_addresses.is_empty() {
                                tracing::debug!(
                                    "🔄 {} inactive masternodes pending reconnection (Phase 3-MN handles every 30s)",
                                    inactive_addresses.len(),
                                );

                                for address in &inactive_addresses {
                                    let pm = health_peer_manager.clone();
                                    let addr = address.clone();
                                    tokio::spawn(async move {
                                        pm.add_peer(addr).await;
                                    });
                                }
                            }
                        }
                    }
                }
            }
        });
        shutdown_manager.register_task(health_handle);

        // ── RUNTIME CHAIN INTEGRITY TASK ─────────────────────────────────────
        // Detect and purge pre-genesis blocks from the stored chain WITHOUT
        // requiring a coordinated restart.  Nodes that were already running when
        // bad blocks 1/2 were written will self-heal on the next tick.
        {
            let integrity_blockchain = blockchain.clone();
            let integrity_shutdown = shutdown_token.clone();
            let integrity_handle = tokio::spawn(async move {
                // Short initial delay — let the node connect to peers first so
                // the subsequent resync request finds someone to talk to.
                tokio::time::sleep(tokio::time::Duration::from_secs(20)).await;
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
                loop {
                    tokio::select! {
                        _ = integrity_shutdown.cancelled() => break,
                        _ = interval.tick() => {
                        let genesis_ts = integrity_blockchain.genesis_timestamp();
                            let stored_height = integrity_blockchain.get_height();
                            if stored_height == 0 {
                                continue;
                            }
                            let purged = integrity_blockchain.purge_pre_genesis_blocks().await;
                            if purged > 0 {
                                tracing::info!(
                                    "✅ Runtime integrity: purged {} pre-genesis block(s), \
                                     chain reset to height 0.",
                                    purged
                                );
                            }
                            let _ = genesis_ts; // suppress unused warning
                        }
                    }
                }
            });
            shutdown_manager.register_task(integrity_handle);
        }
        // ── END RUNTIME CHAIN INTEGRITY TASK ─────────────────────────────────

        // Start masternode announcement task
        let mn_for_announcement = mn.clone();
        let peer_registry_for_announcement = peer_connection_registry.clone();
        let registry_for_announcement = registry.clone();
        let announcement_blockchain = blockchain.clone();
        let signing_key_for_announcement = masternode_signing_key.clone();

        let announcement_handle = tokio::spawn(async move {
            // Wait for initial sync to complete before announcing
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                if !announcement_blockchain.is_syncing() {
                    break;
                }
                tracing::debug!("⏸️ Deferring masternode announcement (still syncing)");
            }

            let daemon_started_at = registry_for_announcement.get_started_at();

            // Build the announcement, using current registry state so that any
            // tier upgrade (e.g. on-chain registration processed after startup)
            // is reflected in re-broadcasts.
            //
            // When a collateral outpoint is configured and masternodeprivkey is
            // available, broadcast V4 with a self-signed collateral proof.  The
            // proof message "TIME_COLLATERAL_CLAIM:<txid>:<vout>" is signed with
            // the masternode key and lets other nodes evict V3 squatters who
            // grabbed the UTXO before the legitimate owner announced.
            let build_announcement = |mn: &types::Masternode| -> NetworkMessage {
                // Never broadcast a collateral outpoint when tier is provisionally Free.
                // Nodes in the deferred-tier state (UTXO not yet resolved) set tier=Free
                // while still carrying the outpoint from config. Peers receiving this
                // contradictory state hit AV40 and can't activate the node. Strip the
                // outpoint so the announcement is internally consistent; the correct tier
                // will be broadcast once the on-chain collateral sync resolves it.
                let effective_outpoint = if mn.tier == types::MasternodeTier::Free {
                    None
                } else {
                    mn.collateral_outpoint.clone()
                };
                if let (Some(ref signing_key), Some(ref outpoint)) =
                    (&signing_key_for_announcement, &effective_outpoint)
                {
                    let txid_hex = hex::encode(outpoint.txid);
                    let proof_msg = format!("TIME_COLLATERAL_CLAIM:{}:{}", txid_hex, outpoint.vout);
                    use ed25519_dalek::Signer;
                    let sig = signing_key.sign(proof_msg.as_bytes());
                    return NetworkMessage::MasternodeAnnouncementV4 {
                        address: mn.address.clone(),
                        reward_address: mn.wallet_address.clone(),
                        tier: mn.tier,
                        public_key: mn.public_key,
                        collateral_outpoint: effective_outpoint,
                        certificate: vec![0u8; 64],
                        started_at: daemon_started_at,
                        collateral_proof: sig.to_bytes().to_vec(),
                    };
                }
                NetworkMessage::MasternodeAnnouncementV3 {
                    address: mn.address.clone(),
                    reward_address: mn.wallet_address.clone(),
                    tier: mn.tier,
                    public_key: mn.public_key,
                    collateral_outpoint: effective_outpoint,
                    certificate: vec![0u8; 64],
                    started_at: daemon_started_at,
                }
            };

            let announcement = build_announcement(&mn_for_announcement);
            peer_registry_for_announcement.broadcast(announcement).await;
            tracing::info!(
                "📢 Broadcast masternode announcement ({}) to network",
                if signing_key_for_announcement.is_some()
                    && mn_for_announcement.collateral_outpoint.is_some()
                {
                    "V4 with collateral proof"
                } else {
                    "V3"
                }
            );

            // Continue broadcasting every 60 seconds; refresh tier from registry
            // so that an on-chain registration that upgrades Free→Bronze is propagated.
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                // Skip during sync (e.g., if a re-sync was triggered)
                if announcement_blockchain.is_syncing() {
                    continue;
                }
                let current_mn = registry_for_announcement
                    .get(&mn_for_announcement.address)
                    .await
                    .map(|info| info.masternode.clone())
                    .unwrap_or_else(|| mn_for_announcement.clone());
                let announcement = build_announcement(&current_mn);
                peer_registry_for_announcement.broadcast(announcement).await;
                tracing::debug!(
                    "📢 Re-broadcast masternode announcement (tier: {:?})",
                    current_mn.tier
                );
            }
        });
        shutdown_manager.register_task(announcement_handle);

        // ── On-chain collateral auto-sync ────────────────────────────────────────
        // After sync completes, compare masternode.conf collateral with the on-chain
        // anchor for this IP.  Submit MasternodeReg / CollateralUnlock transactions
        // as needed so the chain always reflects the current conf state.
        // Run for all tiers so that a downgrade (paid → Free) also triggers unlock.
        {
            let collateral_sync_blockchain = blockchain.clone();
            let collateral_sync_consensus = consensus_engine.clone();
            let collateral_sync_registry = registry.clone();
            let collateral_sync_shutdown = shutdown_token.clone();
            let collateral_sync_utxo = utxo_mgr.clone();
            let mn_for_sync = mn.clone();
            let wallet_key_for_sync = wallet.signing_key().clone();
            let p2p_port_for_sync = network_type.default_p2p_port();

            tokio::spawn(async move {
                // Wait until fully synced before touching the mempool.
                //
                // Important: `is_syncing()` is `false` at startup (before sync starts).
                // We must first wait for sync to BEGIN (flag flips to true), then wait
                // for it to END (flag flips back to false).  If sync never starts within
                // 60 s (e.g. no peers), proceed anyway — this node is alone and needs to
                // self-register now.
                let sync_start_deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(60);
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                    if collateral_sync_shutdown.is_cancelled() {
                        return;
                    }
                    if collateral_sync_blockchain.is_syncing()
                        || std::time::Instant::now() >= sync_start_deadline
                    {
                        break; // Sync has started (or timed out) — now wait for it to finish
                    }
                }
                // Now wait for the active sync to complete.
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    if collateral_sync_shutdown.is_cancelled() {
                        return;
                    }
                    if !collateral_sync_blockchain.is_syncing() {
                        break;
                    }
                    tracing::debug!("⏸️ Deferring collateral sync (still syncing)");
                }
                // Small extra delay so any block-processing finishes
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                if collateral_sync_shutdown.is_cancelled() {
                    return;
                }

                let ip = &mn_for_sync.address;

                // What outpoint is the conf currently advertising?
                let conf_outpoint: Option<String> = mn_for_sync
                    .collateral_outpoint
                    .as_ref()
                    .map(|op| format!("{}:{}", hex::encode(op.txid), op.vout));

                // What outpoint does the chain anchor say is registered for this IP?
                let on_chain_outpoint =
                    collateral_sync_registry.get_on_chain_collateral_outpoint_for_ip(ip);

                tracing::info!(
                    "🔗 Collateral sync: on-chain={:?}, conf={:?}",
                    on_chain_outpoint,
                    conf_outpoint
                );

                if on_chain_outpoint == conf_outpoint && conf_outpoint.is_some() {
                    tracing::info!(
                        "✓ Collateral already registered on-chain: {}",
                        conf_outpoint.as_deref().unwrap_or("-")
                    );
                    return;
                }

                // Deregister the old on-chain record if the collateral outpoint changed
                if let Some(ref _old_outpoint_str) = on_chain_outpoint {
                    if on_chain_outpoint != conf_outpoint {
                        // Look up the slot ID for this node address
                        let slot_id = collateral_sync_registry
                            .get_slot_id_for_address(ip)
                            .await
                            .unwrap_or(u32::MAX);
                        if slot_id == u32::MAX {
                            tracing::warn!(
                                "⚠️ Cannot deregister {}: no slot ID found (not yet confirmed?)",
                                ip
                            );
                        } else {
                            tracing::info!(
                                "📤 Submitting MasternodeDeregistration for {} slot={}",
                                ip,
                                slot_id
                            );
                            if let Some(dereg_tx) =
                                build_masternode_dereg_tx(ip, slot_id, &wallet_key_for_sync)
                            {
                                match collateral_sync_consensus.submit_transaction(dereg_tx).await {
                                    Ok(txid) => tracing::info!(
                                        "✅ MasternodeDeregistration submitted: {} (tx {})",
                                        ip,
                                        hex::encode(txid)
                                    ),
                                    Err(e) => {
                                        tracing::warn!(
                                            "⚠️ MasternodeDeregistration submission failed: {}",
                                            e
                                        )
                                    }
                                }
                            }
                        }
                    }
                }

                // Register the new collateral if conf has one.
                // NOTE: Do NOT gate on mn_for_sync.tier here — it may be provisionally
                // Free if the UTXO lookup failed at startup (chain not yet synced).
                // conf_outpoint.is_some() is the correct guard for paid-tier registration.
                if let Some(ref new_outpoint_str) = conf_outpoint {
                    if on_chain_outpoint.as_deref() != Some(new_outpoint_str.as_str()) {
                        // Before attempting submission, verify that the node's hot wallet key
                        // actually owns the collateral UTXO. The on-chain MasternodeReg
                        // requires a signature from the key whose address matches utxo.address.
                        // If the collateral belongs to a separate cold/GUI wallet, the node
                        // cannot sign on its behalf — the user must submit from that wallet.
                        let can_self_register = if let Some(outpoint) =
                            mn_for_sync.collateral_outpoint.as_ref()
                        {
                            match collateral_sync_utxo.get_utxo(outpoint).await {
                                Ok(utxo) => {
                                    let node_pubkey = wallet_key_for_sync.verifying_key();
                                    let network = collateral_sync_blockchain.network_type();
                                    let node_addr = crate::address::Address::from_public_key(
                                        node_pubkey.as_bytes(),
                                        network,
                                    )
                                    .as_string();
                                    if node_addr == utxo.address {
                                        true
                                    } else {
                                        tracing::warn!(
                                            "⚠️  Cannot auto-submit MasternodeReg: collateral {} \
                                             belongs to address {} but this node's wallet address \
                                             is {}.",
                                            new_outpoint_str,
                                            utxo.address,
                                            node_addr
                                        );
                                        tracing::warn!(
                                            "📋 ACTION REQUIRED: Submit MasternodeReg from the \
                                             wallet that owns the collateral ({}).",
                                            utxo.address
                                        );
                                        tracing::warn!(
                                            "   In your GUI wallet, use: \
                                             Tools → Masternode → Register (or `time-cli masternodereg`)"
                                        );
                                        false
                                    }
                                }
                                Err(_) => {
                                    tracing::warn!(
                                        "⚠️  Cannot look up collateral UTXO {} to verify \
                                         ownership — skipping auto-registration",
                                        new_outpoint_str
                                    );
                                    false
                                }
                            }
                        } else {
                            false
                        };

                        if can_self_register {
                            tracing::info!(
                                "📤 Submitting MasternodeReg for collateral {}",
                                new_outpoint_str
                            );
                            if let Some(reg_tx) = build_masternode_reg_tx(
                                &mn_for_sync,
                                &wallet_key_for_sync,
                                None, // single-key: wallet == operator
                                p2p_port_for_sync,
                            ) {
                                match collateral_sync_consensus.submit_transaction(reg_tx).await {
                                    Ok(txid) => tracing::info!(
                                        "✅ MasternodeReg submitted: {} (tx {})",
                                        new_outpoint_str,
                                        hex::encode(txid)
                                    ),
                                    Err(e) => {
                                        tracing::warn!("⚠️ MasternodeReg submission failed: {}", e)
                                    }
                                }
                            }
                        }
                    }
                }

                // Handle the "collateral commented out" case: on-chain anchor exists
                // but conf has no collateral → already handled by the unlock above
                if on_chain_outpoint.is_some() && conf_outpoint.is_none() {
                    tracing::info!(
                        "ℹ️  Collateral removed from conf — CollateralUnlock submitted above"
                    );
                }

                // ── Free-tier on-chain auto-registration ─────────────────────────────
                // Free-tier nodes have no collateral to register, but after
                // FREE_TIER_ONCHAIN_HEIGHT eligibility for block rewards requires a
                // FreeNodeRegistration TX in the chain.  Submit one automatically on
                // first startup so operators don't need to take any manual action.
                //
                // IMPORTANT: conf_outpoint.is_none() guard prevents a paid-tier node
                // from submitting a Free registration when its tier was provisionally
                // set to Free at startup because the UTXO lookup failed (chain not
                // yet synced).  Without this guard the node would overwrite its own
                // on-chain paid-tier registration with a Free-tier one.
                if mn_for_sync.tier == types::MasternodeTier::Free
                    && conf_outpoint.is_none()
                    && !collateral_sync_blockchain.is_syncing()
                    && !collateral_sync_blockchain.is_masternode_registered(ip)
                {
                    tracing::info!(
                        "📤 Auto-submitting FreeNodeRegistration for {} (wallet: {})",
                        ip,
                        mn_for_sync.wallet_address
                    );
                    if let Some(reg_tx) = build_free_node_reg_tx(
                        ip,
                        &mn_for_sync.wallet_address,
                        &wallet_key_for_sync,
                    ) {
                        match collateral_sync_consensus.submit_transaction(reg_tx).await {
                            Ok(txid) => tracing::info!(
                                "✅ FreeNodeRegistration submitted (tx {})",
                                hex::encode(txid)
                            ),
                            Err(e) => tracing::warn!(
                                "⚠️ FreeNodeRegistration submission failed: {} \
                                 (will retry on next restart)",
                                e
                            ),
                        }
                    }
                }
            });
        }
        // ── End on-chain collateral auto-sync ────────────────────────────────────
    } else {
        // Non-masternode node: still rebuild collateral locks for known peers
        let all_masternodes = registry.list_all().await;
        let lock_height = blockchain.get_height();
        let entries: Vec<_> = all_masternodes
            .iter()
            .filter(|info| {
                !info
                    .masternode
                    .collateral_outpoint
                    .as_ref()
                    .map(|op| stale_local_outpoints.contains(op))
                    .unwrap_or(false)
            })
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

        // One-shot startup purge: drop any stale locks that slipped through rebuild
        // (e.g. gossip entries whose UTXOs are already spent on-chain).
        let startup_purged = consensus_engine.utxo_manager.purge_stale_collateral_locks();
        if startup_purged > 0 {
            tracing::warn!(
                "🧹 [STARTUP] Purged {} stale collateral lock(s) after registry rebuild",
                startup_purged
            );
        }
    }

    // Spawn background task that updates last_block_time whenever the chain grows.
    {
        let lbt = last_block_time.clone();
        let bc = blockchain.clone();
        let mut last_seen_height = bc.get_height();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let h = bc.get_height();
                if h != last_seen_height {
                    last_seen_height = h;
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    lbt.store(now, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
    }

    // Spawn background collateral lock sweep (every 60 s).
    // Removes stale locks for UTXOs that are no longer Unspent, keeping all nodes'
    // lock sets consistent with the chain regardless of gossip race conditions.
    {
        let utxo_mgr = consensus_engine.utxo_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let purged = utxo_mgr.purge_stale_collateral_locks();
                if purged > 0 {
                    tracing::warn!(
                        "🧹 [LOCK-SWEEP] Background sweep purged {} stale collateral lock(s)",
                        purged
                    );
                }
            }
        });
    }

    // Spawn partition detector — monitors peer count and block staleness.
    {
        let detector = network::partition_detector::PartitionDetector::new(
            peer_connection_registry.clone(),
            registry.clone(),
            peer_manager.clone(),
            config.network.bootstrap_peers.clone(),
            network_type,
            last_block_time.clone(),
            local_ip.clone(),
        );
        tokio::spawn(async move { detector.run().await });
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
            const PEER_WAIT_SECS: u64 = 5;
            const GENESIS_WAIT_SECS: u64 = 10;

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

                // AV31: request from whitelisted (trusted) peers first; only fall back to
                // unwhitelisted peers if whitelisted ones don't respond in time.
                // This prevents an attacker who controls the majority of initial connections
                // from injecting a forked genesis.
                let whitelisted = peer_registry_for_sync
                    .get_whitelisted_connected_peers()
                    .await;
                let priority_peers: Vec<&String> = if whitelisted.is_empty() {
                    connected.iter().collect()
                } else {
                    tracing::info!(
                        "🔐 Requesting genesis from {} whitelisted peer(s) first (AV31)",
                        whitelisted.len()
                    );
                    whitelisted.iter().collect()
                };

                for peer_ip in &priority_peers {
                    let msg = crate::network::message::NetworkMessage::RequestGenesis;
                    let _ = peer_registry_for_sync.send_to_peer(peer_ip, msg).await;
                }

                // Wait for genesis from whitelisted peers
                let mut wait_secs = 0;
                while wait_secs < GENESIS_WAIT_SECS {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    if blockchain_init.has_genesis() {
                        tracing::info!("✅ Successfully synced genesis block from network");
                        break;
                    }
                    wait_secs += 1;
                }

                // Fallback: if whitelisted peers didn't deliver, ask the rest
                if !blockchain_init.has_genesis() && !whitelisted.is_empty() {
                    let fallback: Vec<&String> = connected
                        .iter()
                        .filter(|ip| !whitelisted.contains(ip))
                        .collect();
                    if !fallback.is_empty() {
                        tracing::info!(
                            "📥 Falling back to {} non-whitelisted peer(s) for genesis",
                            fallback.len()
                        );
                        for peer_ip in &fallback {
                            let msg = crate::network::message::NetworkMessage::RequestGenesis;
                            let _ = peer_registry_for_sync.send_to_peer(peer_ip, msg).await;
                        }
                        // Brief wait for fallback response
                        for _ in 0..3 {
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            if blockchain_init.has_genesis() {
                                break;
                            }
                        }
                    }
                }
            }

            // Phase 2: If still no genesis, this is a new network - generate dynamically
            if !blockchain_init.has_genesis() {
                tracing::info!("🌱 No genesis on network - initiating dynamic generation");

                // ── LAUNCH-TIME CLOCK GUARD ───────────────────────────────────────────
                // Block here until the official launch time is reached before attempting
                // genesis generation.  generate_dynamic_genesis() has its own hard stop
                // too, but this outer wait avoids spamming error logs and lets nodes
                // synchronise naturally at the moment of launch.
                {
                    let launch_ts = blockchain_init.genesis_timestamp();
                    loop {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        if now >= launch_ts {
                            break;
                        }
                        // Check if a peer already delivered genesis while we were waiting
                        if blockchain_init.has_genesis() {
                            tracing::info!(
                                "✅ Genesis block received from network before launch time"
                            );
                            break;
                        }
                        let remaining = launch_ts - now;
                        let launch_str = chrono::DateTime::from_timestamp(launch_ts, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                            .unwrap_or_else(|| launch_ts.to_string());
                        tracing::info!(
                            "⏰ Waiting for launch time: {}s remaining until {} ...",
                            remaining,
                            launch_str
                        );
                        // Sleep in 60s increments so we log progress each minute
                        let sleep_secs = remaining.min(60) as u64;
                        tokio::time::sleep(tokio::time::Duration::from_secs(sleep_secs)).await;
                    }
                }

                // If genesis arrived from the network during the wait, skip generation
                if blockchain_init.has_genesis() {
                    // fall through — the code below already handles this case
                } else {
                    // Wait for masternodes to discover each other with exponential backoff
                    // Start with 30s, then 60s, then 90s - total 180s max wait
                    const DISCOVERY_ROUNDS: u32 = 3;
                    const BASE_DISCOVERY_WAIT: u64 = 10;

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
                            tracing::info!(
                                "👑 We are the genesis leader - generating genesis block"
                            );

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
                            tracing::info!(
                                "⏳ Waiting for genesis from leader ({})",
                                leader_address
                            );

                            const LEADER_WAIT_SECS: u64 = 45;
                            const REQUEST_INTERVAL: u64 = 10;
                            let mut waited = 0u64;

                            while waited < LEADER_WAIT_SECS && !blockchain_init.has_genesis() {
                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                waited += 1;

                                // Re-request genesis periodically — whitelist-first (AV31)
                                if waited % REQUEST_INTERVAL == 0 {
                                    let connected =
                                        peer_registry_for_sync.get_connected_peers().await;
                                    let wl = peer_registry_for_sync
                                        .get_whitelisted_connected_peers()
                                        .await;
                                    let targets: Vec<&String> = if wl.is_empty() {
                                        connected.iter().collect()
                                    } else {
                                        wl.iter().collect()
                                    };
                                    for peer_ip in targets {
                                        let msg =
                                            crate::network::message::NetworkMessage::RequestGenesis;
                                        let _ =
                                            peer_registry_for_sync.send_to_peer(peer_ip, msg).await;
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
                                    tracing::error!(
                                        "❌ Failed to generate fallback genesis: {}",
                                        e
                                    );
                                } else if let Ok(genesis) =
                                    blockchain_init.get_block_by_height(0).await
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
                } // end else (launch time reached, generate locally)
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

        // Re-sync watchdog: if the node stalls (height not advancing but peers are ahead),
        // retry sync every 5 minutes so a post-initial-sync stall self-recovers.
        let blockchain_for_watchdog = blockchain_init.clone();
        tokio::spawn(async move {
            // Give the node time to settle before first watchdog check
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
            loop {
                interval.tick().await;

                let current = blockchain_for_watchdog.get_height();

                // Check if any peer is ahead of us
                let max_peer_height = if let Some(reg) =
                    blockchain_for_watchdog.get_peer_registry().await
                {
                    let peers = reg.get_connected_peers().await;
                    let mut max_h = current;
                    let mut tips_found = 0usize;
                    for peer in &peers {
                        if let Some((h, _)) = reg.get_peer_chain_tip(peer).await {
                            tips_found += 1;
                            if h > max_h {
                                max_h = h;
                            }
                        }
                    }
                    // If we have connected peers but NO cached chain tips, the tips cache
                    // was cleared by peer churn and hasn't been repopulated yet.
                    // Broadcast GetChainTip so the cache is refreshed before next check.
                    if !peers.is_empty() && tips_found == 0 {
                        tracing::warn!(
                            "🔁 Sync watchdog: {} peer(s) connected but tip cache is empty — requesting chain tips",
                            peers.len()
                        );
                        let _ = reg
                            .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                            .await;
                    }
                    max_h
                } else {
                    current
                };

                if max_peer_height > current + 1 {
                    tracing::warn!(
                        "🔁 Sync watchdog: height {} is behind best peer at {} — retrying sync",
                        current,
                        max_peer_height
                    );
                    if let Err(e) = blockchain_for_watchdog
                        .sync_from_peers(Some(max_peer_height))
                        .await
                    {
                        tracing::warn!("⚠️ Watchdog sync attempt failed: {}", e);
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

                // Skip during sync — chain is incomplete
                if blockchain_for_integrity.is_syncing() {
                    continue;
                }

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

        // PEER GATE: Require at least 3 compatible peers before producing any blocks.
        // This prevents a lone node from producing a chain that forks from the network,
        // and ensures that peers with invalid/wrong-chain blocks (different genesis) are
        // excluded from consideration since they won't appear in get_compatible_peers().
        const MIN_PEERS_FOR_PRODUCTION: usize = 3;
        const PEER_GATE_LOG_INTERVAL_SECS: u64 = 30;
        const PEER_GATE_FALLBACK_SECS: u64 = 600; // 10 min — allow solo if truly isolated

        {
            let mut peer_gate_wait = 0u64;
            tracing::info!(
                "🔒 Peer gate: waiting for {} compatible peer(s) before block production",
                MIN_PEERS_FOR_PRODUCTION
            );
            loop {
                let compatible = block_peer_registry.get_compatible_peers().await;
                if compatible.len() >= MIN_PEERS_FOR_PRODUCTION {
                    tracing::info!(
                        "🔓 Peer gate passed: {} compatible peer(s) connected (waited {}s)",
                        compatible.len(),
                        peer_gate_wait
                    );
                    break;
                }

                if peer_gate_wait >= PEER_GATE_FALLBACK_SECS {
                    tracing::warn!(
                        "⚠️ Peer gate timeout after {}s with only {}/{} compatible peer(s) — \
                         proceeding solo (network may be small or starting up)",
                        peer_gate_wait,
                        compatible.len(),
                        MIN_PEERS_FOR_PRODUCTION
                    );
                    break;
                }

                if peer_gate_wait % PEER_GATE_LOG_INTERVAL_SECS == 0 && peer_gate_wait > 0 {
                    tracing::info!(
                        "⏳ Peer gate: {}/{} compatible peer(s) connected ({}s elapsed, \
                         timeout in {}s)",
                        compatible.len(),
                        MIN_PEERS_FOR_PRODUCTION,
                        peer_gate_wait,
                        PEER_GATE_FALLBACK_SECS - peer_gate_wait
                    );
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                peer_gate_wait += 2;
            }
        }

        // CATCH-UP GATE: A freshly-started (or long-offline) node must finish catching
        // up to the network BEFORE producing any blocks or participating in consensus.
        // Producing while hundreds of blocks behind creates a fork — we cannot validate
        // recent votes (registry is stale), we cannot select the correct leader, and
        // the blocks we produce would be rejected by the rest of the network.
        //
        // This gate blocks here (re-requesting missing blocks every REQUEST_INTERVAL
        // seconds) until we are within CATCHUP_THRESHOLD blocks of both the time-based
        // expected height AND the highest height reported by connected peers.
        // With 10-minute block times there is no "latency tolerance" — if a peer is
        // 1 block ahead it has a 10-minute-old block we simply do not have.  A node
        // must be at exactly the network tip before producing.  Threshold=0 enforces
        // this: production is gated until behind==0 (we match every connected peer).
        const CATCHUP_THRESHOLD: u64 = 0;
        const CATCHUP_LOG_INTERVAL_SECS: u64 = 15;
        const CATCHUP_REQUEST_INTERVAL_SECS: u64 = 10;
        const MIN_CONFIRMED_PEERS: usize = 3;

        {
            let mut catchup_wait: u64 = 0;
            let mut last_request: u64 = 0;
            let mut announced_gate = false;

            loop {
                let current = block_blockchain.get_height();
                let expected = block_blockchain.calculate_expected_height();

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

                let target = expected.max(max_reported_height);
                let behind = target.saturating_sub(current);

                // No peer has blocks beyond our height — the whole network is stalled.
                // We cannot obtain missing blocks from anyone; we must produce to advance.
                let no_peer_ahead = max_reported_height <= current;

                if peers_with_data >= MIN_CONFIRMED_PEERS
                    && (behind == CATCHUP_THRESHOLD || no_peer_ahead)
                {
                    if no_peer_ahead && behind > 0 {
                        tracing::warn!(
                            "⚠️  Catch-up gate: no peer has blocks beyond height {} \
                             (time-based expected {}). Network stall detected — \
                             releasing gate to allow local production.",
                            current,
                            expected
                        );
                    } else {
                        tracing::info!(
                            "🔓 Catch-up gate passed: height {} (network tip {}, expected {}, waited {}s)",
                            current, max_reported_height, expected, catchup_wait
                        );
                    }
                    break;
                }

                if !announced_gate {
                    announced_gate = true;
                    tracing::info!(
                        "🔒 Catch-up gate: {} blocks behind (current {}, network {}, expected {}) — \
                         suspending block production and consensus participation until synced",
                        behind, current, max_reported_height, expected
                    );
                }

                if peers_with_data < MIN_CONFIRMED_PEERS {
                    if catchup_wait % CATCHUP_LOG_INTERVAL_SECS == 0 && catchup_wait > 0 {
                        tracing::info!(
                            "⏳ Catch-up gate: waiting for peer chain data ({}s elapsed, {} peers connected)",
                            catchup_wait, peers.len()
                        );
                    }
                } else {
                    if catchup_wait % CATCHUP_LOG_INTERVAL_SECS == 0 && catchup_wait > 0 {
                        tracing::info!(
                            "⏳ Catch-up gate: {} blocks behind (current {} / network {}), waited {}s",
                            behind, current, max_reported_height, catchup_wait
                        );
                    }

                    // Periodically nudge peers for missing blocks — sync_from_peers
                    // runs on its own coordinator schedule, but explicit GetBlocks
                    // requests here accelerate catch-up when it's idle.
                    if catchup_wait.saturating_sub(last_request) >= CATCHUP_REQUEST_INTERVAL_SECS
                        && !block_blockchain.is_syncing()
                    {
                        last_request = catchup_wait;
                        let requested_end = max_reported_height.min(current + 50);
                        if requested_end > current {
                            for peer_ip in &peers {
                                let msg = crate::network::message::NetworkMessage::GetBlocks(
                                    current + 1,
                                    requested_end,
                                );
                                let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                            }
                        }
                    }
                }

                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                catchup_wait += 2;
            }
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
        const LEADER_TIMEOUT_SECS: u64 = 10; // Match validator's 10s relaxation interval
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

        // Minority-chain check: compare our tip hash against peers even when at same height.
        // Run every 30 s to detect same-height forks without hammering compare_chain_with_peers().
        let mut last_minority_check =
            std::time::Instant::now() - std::time::Duration::from_secs(30); // fire immediately on first loop

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

            // Skip block production entirely while syncing — focus on catching up
            if block_blockchain.is_syncing() {
                continue;
            }

            // Periodically broadcast GetChainTip so peer heights stay fresh.
            // This MUST run before the catch-up `continue` below so that nodes
            // which are behind still discover peer heights and can request blocks.
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

            // Fork-state watchdog: if handle_fork() entered FetchingChain and the
            // triggering peer then disconnected, no new blocks arrive to trigger
            // the in-function stall detector.  Clear the state here after 90 s so
            // the next chain-tip comparison can start a fresh resolution attempt.
            {
                use crate::blockchain::ForkResolutionState;
                let stale = {
                    let fs = block_blockchain.fork_state.read().await;
                    match &*fs {
                        ForkResolutionState::FetchingChain { started_at, .. } => {
                            started_at.elapsed() > std::time::Duration::from_secs(90)
                        }
                        ForkResolutionState::Reorging { started_at, .. } => {
                            started_at.elapsed() > std::time::Duration::from_secs(30)
                        }
                        _ => false,
                    }
                };
                if stale {
                    tracing::warn!(
                        "⚠️  Fork state watchdog: clearing stale fork resolution state \
                         (peer likely disconnected mid-resolution)"
                    );
                    *block_blockchain.fork_state.write().await = ForkResolutionState::None;
                }
            }

            // FORK STATE GUARD: If the fork resolution state machine is active
            // (FetchingChain, ReadyToReorg, Reorging), halt production until it completes.
            // Producing blocks while a reorg is in progress would extend the wrong chain.
            {
                use crate::blockchain::ForkResolutionState;
                let in_fork_resolution = !matches!(
                    *block_blockchain.fork_state.read().await,
                    ForkResolutionState::None
                );
                if in_fork_resolution {
                    tracing::debug!("⏸️  Production paused: fork resolution in progress");
                    continue;
                }
            }

            // MINORITY CHAIN GUARD: Even when at exactly the same height as peers, we may
            // be on a minority fork (same height, different tip hash).  The runtime
            // catch-up check above only fires when we are *behind* — it cannot catch this
            // case.  Periodically compare our tip hash against the majority.  If the
            // majority is on a different chain, stop producing and trigger a sync/reorg.
            if last_minority_check.elapsed() >= std::time::Duration::from_secs(30) {
                last_minority_check = std::time::Instant::now();
                if let Some((consensus_height, _)) =
                    block_blockchain.compare_chain_with_peers().await
                {
                    let cur = block_blockchain.get_height();
                    tracing::warn!(
                        "🔀 Minority chain detected (our height {}, consensus height {}) \
                         — pausing production, requesting sync to canonical chain",
                        cur,
                        consensus_height
                    );
                    if let Err(e) = block_blockchain
                        .sync_from_peers(Some(consensus_height))
                        .await
                    {
                        tracing::warn!("⚠️  Fork sync failed: {}", e);
                    }
                    continue;
                }
            }

            // Runtime catch-up safety: if we've fallen significantly behind the network
            // mid-run (e.g., sustained network outage, big reorg), pause production and
            // let sync catch us back up.  Same CATCHUP_THRESHOLD as the startup gate.
            {
                let cur = block_blockchain.get_height();
                let exp = block_blockchain.calculate_expected_height();
                let peers = block_peer_registry.get_compatible_peers().await;
                let mut peer_max = 0u64;
                for p in &peers {
                    if let Some(h) = block_peer_registry.get_peer_height(p).await {
                        peer_max = peer_max.max(h);
                    } else if let Some((h, _)) = block_peer_registry.get_peer_chain_tip(p).await {
                        peer_max = peer_max.max(h);
                    }
                }
                let target = exp.max(peer_max);
                let behind = target.saturating_sub(cur);
                // Only pause production when a peer *actually has* blocks we don't.
                // If `behind` is purely time-based (peer_max <= cur), every peer is at
                // our height or below — the network is stalled and we must produce to
                // advance the chain. Blocking here in that case causes a deadlock where
                // nobody produces because everyone is waiting for someone else to produce.
                if behind > 0 && peer_max > cur {
                    static LAST_BEHIND_LOG: std::sync::atomic::AtomicI64 =
                        std::sync::atomic::AtomicI64::new(0);
                    let now_secs = chrono::Utc::now().timestamp();
                    let last = LAST_BEHIND_LOG.load(Ordering::Relaxed);
                    if now_secs - last >= 30 {
                        LAST_BEHIND_LOG.store(now_secs, Ordering::Relaxed);
                        tracing::info!(
                            "⏸️  Pausing production: {} blocks behind (current {}, network {}, expected {}) — waiting for sync",
                            behind, cur, peer_max, exp
                        );
                    }
                    // Request more blocks from peers to accelerate sync
                    if !peers.is_empty() {
                        let requested_end = peer_max.min(cur + 50);
                        for peer_ip in &peers {
                            let msg = crate::network::message::NetworkMessage::GetBlocks(
                                cur + 1,
                                requested_end,
                            );
                            let _ = block_peer_registry.send_to_peer(peer_ip, msg).await;
                        }
                    }
                    continue;
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
                // Rate-limit bootstrap log to avoid spam when stuck at height 0
                static LAST_BOOTSTRAP_LOG: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last = LAST_BOOTSTRAP_LOG.load(Ordering::Relaxed);
                if now_secs - last >= 30 {
                    LAST_BOOTSTRAP_LOG.store(now_secs, Ordering::Relaxed);
                    tracing::info!(
                        "🌱 Bootstrap mode (height {}): using ALL {} registered masternodes (including inactive, no bitmap yet)",
                        current_height,
                        all_nodes.len()
                    );
                }
                // CONSENSUS GUARD: require ≥3 registered masternodes before producing
                // block 1.  A single node must never be able to produce block 1 alone —
                // the reward distribution would have only 1 recipient, which violates the
                // ≥3 recipients rule and would split the chain as other nodes reject it.
                const MIN_MASTERNODES_FOR_BLOCK1: usize = 3;
                if all_nodes.len() < MIN_MASTERNODES_FOR_BLOCK1 {
                    static LAST_WAIT_LOG: std::sync::atomic::AtomicI64 =
                        std::sync::atomic::AtomicI64::new(0);
                    let now_secs = chrono::Utc::now().timestamp();
                    let last = LAST_WAIT_LOG.load(Ordering::Relaxed);
                    if now_secs - last >= 30 {
                        LAST_WAIT_LOG.store(now_secs, Ordering::Relaxed);
                        tracing::info!(
                            "⏳ Bootstrap: waiting for ≥{} masternodes before producing block 1 \
                             ({} registered so far)",
                            MIN_MASTERNODES_FOR_BLOCK1,
                            all_nodes.len()
                        );
                    }
                    continue;
                }

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

                if prev_block.is_some() {
                    // VRF eligibility: rolling 3-block participation window.
                    //
                    // A node is eligible if it appeared in the bitmap (or was the block
                    // producer) of ANY of the last 3 blocks.  This means:
                    //   - High-latency nodes whose vote arrived late for block N but was
                    //     present in block N-1 are still eligible (not unfairly penalised).
                    //   - Nodes that just joined and haven't voted in any recent block are
                    //     excluded (preserving the participation incentive).
                    //   - A node must miss 3 consecutive rounds before losing VRF eligibility.
                    const VRF_WINDOW: u64 = 3;
                    let mut participant_union: std::collections::HashSet<String> =
                        std::collections::HashSet::new();

                    for lookback in 0..VRF_WINDOW {
                        if current_height < lookback {
                            break;
                        }
                        let check_height = current_height - lookback;
                        if let Ok(blk) = block_blockchain.get_block_by_height(check_height).await {
                            // Block producer always participated
                            if !blk.header.leader.is_empty() {
                                participant_union.insert(blk.header.leader.clone());
                            }
                            // All bitmap voters
                            let voters = block_registry
                                .get_active_from_bitmap(&blk.header.active_masternodes_bitmap)
                                .await;
                            for v in voters {
                                participant_union.insert(v.masternode.address.clone());
                            }
                        }
                    }

                    let all_active = block_registry.get_eligible_for_rewards().await;

                    if participant_union.is_empty() {
                        // No participation data yet — fall through to all-active
                        tracing::warn!(
                            "⚠️  Rolling bitmap window empty at height {} - using all active masternodes",
                            current_height
                        );
                        all_active
                    } else {
                        let filtered: Vec<_> = all_active
                            .into_iter()
                            .filter(|(mn, _)| participant_union.contains(&mn.address))
                            .collect();

                        tracing::debug!(
                            "📊 VRF pool: {}/{} active masternodes participated in last {} blocks",
                            filtered.len(),
                            participant_union.len(),
                            VRF_WINDOW
                        );

                        if filtered.is_empty() {
                            // Safety: avoid deadlock if bitmap data is inconsistent
                            block_registry.get_eligible_for_rewards().await
                        } else {
                            filtered
                        }
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
                    // Emergency fallback: when severely stalled (≥5 blocks behind), use ALL
                    // registered masternodes regardless of active status. After 50+ minutes
                    // without a block, the "active" gossip state is stale and we must restart
                    // the chain. This implements the "emergency fallback to all registered"
                    // step promised in the comment above.
                    if blocks_behind >= 5 {
                        let all_registered = block_registry.list_all().await;
                        let registered_masternodes: Vec<crate::types::Masternode> = all_registered
                            .iter()
                            .map(|info| info.masternode.clone())
                            .collect();
                        if registered_masternodes.len() >= 3 {
                            static LAST_EMERGENCY_LOG: std::sync::atomic::AtomicI64 =
                                std::sync::atomic::AtomicI64::new(0);
                            let now_secs = chrono::Utc::now().timestamp();
                            let last = LAST_EMERGENCY_LOG.load(Ordering::Relaxed);
                            if now_secs - last >= 60 {
                                LAST_EMERGENCY_LOG.store(now_secs, Ordering::Relaxed);
                                tracing::warn!(
                                    "🚨 Emergency fallback: {} active masternodes < 3 but {} blocks behind. \
                                     Using {} registered masternodes for VRF.",
                                    masternodes.len(),
                                    blocks_behind,
                                    registered_masternodes.len()
                                );
                            }
                            masternodes = registered_masternodes;
                            // Fall through to production
                        } else {
                            // Truly no masternodes registered — cannot produce
                            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                            continue;
                        }
                    } else {
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
                        // Back off to avoid spinning every second
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        continue;
                    }
                }
            }

            // Double-check we have enough masternodes after fallback logic
            if masternodes.len() < 3 {
                // Rate-limit this warning (once per 60s)
                static LAST_INSUF_WARN: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last = LAST_INSUF_WARN.load(Ordering::Relaxed);
                if now_secs - last >= 60 {
                    LAST_INSUF_WARN.store(now_secs, Ordering::Relaxed);
                    tracing::warn!(
                        "⚠️ Insufficient masternodes ({}) for block production - skipping",
                        masternodes.len()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            }

            // Additional safety: check masternodes is not empty to prevent panic
            if masternodes.is_empty() {
                tracing::error!(
                    "🛡️ FORK PREVENTION: Empty masternode set - refusing block production"
                );
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
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
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
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
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
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

                    // STALL GUARD: Before calling compare_chain_with_peers() (which reads
                    // cached tips), verify that we actually have tip data for at least one
                    // connected peer. If the cache is empty — e.g. after peer churn cleared
                    // it — compare_chain_with_peers returns None, which the code below
                    // interprets as "peers agree." That interpretation is WRONG when the
                    // cache is simply cold; it causes the node to skip syncing and spin
                    // indefinitely at a stale height. Detect empty cache early and refill it.
                    let any_tip_cached = {
                        let mut found = false;
                        for peer_ip in &connected_peers {
                            if block_peer_registry
                                .get_peer_chain_tip(peer_ip)
                                .await
                                .is_some()
                            {
                                found = true;
                                break;
                            }
                        }
                        found
                    };
                    if !any_tip_cached {
                        tracing::warn!(
                            "⚠️  {} blocks behind, {} peer(s) connected but tip cache is empty \
                             — requesting chain tips before sync decision",
                            blocks_behind,
                            connected_peers.len()
                        );
                        block_peer_registry
                            .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                            .await;
                        last_chain_tip_request = std::time::Instant::now();
                        // Brief wait for tip responses to arrive before looping back
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    }

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
                            // Yield for 5 seconds before re-checking. sync_from_peers() can
                            // return Ok(()) immediately when no peers are ahead, causing a tight
                            // busy-loop that starves the tokio runtime and hangs the RPC handler.
                            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
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
                        // Yield for 5 seconds before re-checking. sync_from_peers() can
                        // return Ok(()) immediately when no peers are ahead, causing a tight
                        // busy-loop that starves the tokio runtime and hangs the RPC handler.
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
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
                // Pre-boost leader_attempt when behind so VRF threshold relaxes immediately.
                // >50 blocks behind: start at attempt 3 (fully relaxed, fastest catch-up)
                // >10 blocks behind: start at attempt 2 (2x weight boost + relaxation)
                // Otherwise: start at attempt 0 (normal strictness)
                leader_attempt = if blocks_behind > 50 {
                    3
                } else if blocks_behind > 10 {
                    2
                } else {
                    0
                };
                height_first_seen = std::time::Instant::now();
            }
            let slot_elapsed_secs = time_past_scheduled.max(0) as u64;
            let wall_elapsed_secs = height_first_seen.elapsed().as_secs();
            let effective_elapsed = slot_elapsed_secs.min(wall_elapsed_secs);
            // Use shorter timeout when catching up for faster block production
            let timeout_secs = if blocks_behind > 50 {
                2
            } else {
                LEADER_TIMEOUT_SECS
            };
            let expected_attempt = effective_elapsed / timeout_secs;
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
            // Fairness bonus: +1 per 10 blocks without reward, uncapped
            // This ensures nodes that haven't produced blocks get increasing priority.
            // Reads from the in-memory counter updated by add_block — O(n), no sled scan.
            let blocks_without_reward_map = block_registry.get_reward_tracking_from_memory().await;

            let our_blocks_without = blocks_without_reward_map
                .get(&our_addr)
                .copied()
                .unwrap_or(0);
            let our_fairness_bonus = our_blocks_without / 10;
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
                        .map(|b| b / 10)
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
            // SECURITY: Free tier nodes only get emergency boost after extended deadlock.
            // Validator uses `elapsed / 10` intervals; with capped weight 9 and a ~2338-weight
            // network, Free tier needs multiplier ≥32 (attempt≥5, 50s) to pass validation.
            // Gate at attempt≥5 so the producer only self-selects when the validator will accept.
            let effective_sampling_weight = if leader_attempt > 0 {
                let allow_boost = if matches!(our_mn.tier, crate::types::MasternodeTier::Free) {
                    leader_attempt >= 5 // Free tier: 50s deadlock (aligns with validator 2^5=32x)
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
            // Rate-limit this log when far behind: we're eligible every second but
            // can't produce (e.g., not enough peers), so cap to once per 30s.
            {
                static LAST_PROPOSER_LOG: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last = LAST_PROPOSER_LOG.load(Ordering::Relaxed);
                if blocks_behind < 5 || now_secs - last >= 30 {
                    LAST_PROPOSER_LOG.store(now_secs, Ordering::Relaxed);
                    tracing::info!(
                        "🎯 VRF selected as block proposer for height {} (score: {}, {}s past scheduled time)",
                        next_height,
                        vrf_score,
                        time_past_scheduled
                    );
                }
            }

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
                            if leader_attempt >= 15 {
                                // Stalled proposal: 150+ seconds with no peer consensus.
                                // Evict the stale cached entry so the production flow runs
                                // again with a fresh block, and the extended-deadlock solo
                                // fallback (leader_attempt ≥ 15 → allow solo add_block) fires.
                                tracing::warn!(
                                    "⏱️  Cached proposal for height {} stalled {} attempts, \
                                     evicting for fresh repropose",
                                    next_height,
                                    leader_attempt
                                );
                                cache.remove(&existing.hash());
                                // Fall through to production flow
                            } else {
                                // Our own cached proposal — already produced and broadcast
                                tracing::debug!(
                                    "⏭️  Already proposed block for height {}, waiting for consensus",
                                    next_height,
                                );
                                continue;
                            }
                        } else if existing.header.vrf_score < vrf_score && leader_attempt == 0 {
                            // A peer's proposal has a strictly better (lower) VRF score.
                            // Only defer on the first attempt — after one timeout window the peer
                            // has had their chance and failed to produce, so we override.
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
            // Require at least 1/3 of COMPATIBLE peers as connected, with a hard
            // floor of 2, to prevent isolated nodes from creating forks.
            // Floor is 2 (not 3) so a 4-node testnet (3 other peers) can produce
            // when connected to 2/3 of them — that is still a majority.
            // NOTE: We use compatible peer count (not total active masternodes) because
            // banned/incompatible nodes should not inflate the quorum threshold.
            // After a chain restart, most old-code nodes will be banned — the quorum
            // must reflect only the nodes we can actually talk to.
            let connected_peers = block_peer_registry.get_compatible_peers().await;
            let compatible_count = connected_peers.len();
            let min_peers_required = (compatible_count / 3).max(2);
            if compatible_count < min_peers_required {
                // Rate-limit to once per 30s — this fires every second when syncing
                static LAST_PEER_WARN: std::sync::atomic::AtomicI64 =
                    std::sync::atomic::AtomicI64::new(0);
                let now_secs = chrono::Utc::now().timestamp();
                let last = LAST_PEER_WARN.load(Ordering::Relaxed);
                if now_secs - last >= 30 {
                    LAST_PEER_WARN.store(now_secs, Ordering::Relaxed);
                    tracing::warn!(
                        "⚠️ Only {} compatible peer(s) (need {}) - waiting for more peers before producing",
                        compatible_count,
                        min_peers_required,
                    );
                }
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
            // Skip fork-prevention at genesis: compatible peers may still report their
            // old-chain heights (94-96) from pings before we've had a chance to download
            // and reject their block 1. At height 0 there is no local chain to protect,
            // so blocking production here would trap the node forever.
            if current_height > 0 && max_peer_height_final > current_height {
                // Override after repeated failed attempts: if peers have been reporting
                // a higher height for many consecutive timeouts (≥5 attempts = ~50s) but
                // every block they send is rejected as invalid (bad pool distribution,
                // reward injection, etc.), stop waiting for them and produce our own block.
                // This prevents permanent stall when old-code nodes report fake heights.
                if leader_attempt >= 5 {
                    tracing::warn!(
                        "⚠️ Fork-prevention override (attempt {}): peers report height {} but \
                         all their blocks have been invalid — producing our own block",
                        leader_attempt,
                        max_peer_height_final
                    );
                    // Fall through to block production below
                } else {
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
            // During catch-up (far behind), skip this check — TimeLock consensus on
            // each individual block already proves peer agreement. This check only
            // matters at chain tip to prevent solo production.
            if blocks_behind <= 10 && !block_blockchain.check_2_3_consensus_cached().await {
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
                        "⏱️ No majority peer consensus for block {} after 10s — triggering sync instead",
                        next_height
                    );
                    // Instead of just skipping, actively try to sync.
                    // If peers are ahead of us, we should catch up rather than
                    // endlessly retrying block production.
                    let sync_bc = block_blockchain.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sync_bc.sync_from_peers(None).await {
                            tracing::debug!("Sync after consensus failure: {}", e);
                        }
                    });
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
                            // Broadcast our precommit vote — compute signature first so it can
                            // also be stored in the accumulator alongside the vote weight.
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
                            block_consensus_engine.timevote.generate_precommit_vote(
                                block_hash,
                                our_addr,
                                our_weight,
                                precommit_sig.clone(),
                            );
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

                    let consensus_timeout = if blocks_behind > 50 {
                        std::time::Duration::from_secs(2) // Far behind: rapid catch-up
                    } else if blocks_behind > 10 {
                        std::time::Duration::from_secs(5) // Slightly behind: fast catch-up
                    } else if blocks_behind > 0 {
                        std::time::Duration::from_secs(10) // Behind: shorter timeout
                    } else {
                        std::time::Duration::from_secs(15) // Normal: wait for consensus signal
                    };

                    let block_signal = block_blockchain.block_added_signal();
                    let vote_notify = block_consensus_engine.timevote.vote_notify.clone();

                    // Wait for: block committed by message handler (full BFT path),
                    // precommit majority reached via vote accumulation (event-driven early
                    // exit), or timeout (solo fallback when peers don't participate).
                    // The select! wakes immediately on any vote arrival so consensus
                    // completes without sleeping through the full timeout when votes are
                    // flowing normally.
                    let consensus_reached = tokio::time::timeout(consensus_timeout, async {
                        loop {
                            tokio::select! {
                                _ = block_signal.notified() => {
                                    // Check if OUR block was the one added
                                    if block_blockchain.get_height() >= block_height {
                                        return true;
                                    }
                                    // Signal was for a different block, keep waiting
                                }
                                _ = vote_notify.notified() => {
                                    if block_blockchain.get_height() >= block_height {
                                        // Already added (race with block_signal path)
                                        return true;
                                    }
                                    if block_consensus_engine
                                        .timevote
                                        .check_precommit_consensus(block_hash)
                                    {
                                        // Precommit majority reached — caller adds block
                                        return false;
                                    }
                                    // Not enough votes yet, keep waiting
                                }
                            }
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
                        Ok(false) => {
                            // Event-driven: precommit majority accumulated before the
                            // message handler completed its own add_block call.
                            let current_height = block_blockchain.get_height();
                            if current_height >= block_height {
                                tracing::debug!(
                                    "✅ Block {} already finalized (chain at {}), skipping vote-majority add",
                                    block_height,
                                    current_height
                                );
                            } else {
                                tracing::info!(
                                    "⚡ Block {} adding via vote majority (precommit consensus reached)",
                                    block_height
                                );
                                if let Err(e) = block_blockchain.add_block(block.clone()).await {
                                    tracing::error!(
                                        "❌ Failed to add block via vote majority: {}",
                                        e
                                    );
                                } else {
                                    let finalized_msg = crate::network::message::NetworkMessage::TimeLockBlockProposal {
                                        block: block.clone(),
                                    };
                                    block_peer_registry.broadcast(finalized_msg).await;
                                    tracing::info!(
                                        "✅ Block {} added via vote majority, broadcast to peers",
                                        block_height
                                    );
                                }
                            }
                            block_consensus_engine
                                .timevote
                                .cleanup_block_votes(block_hash);
                        }
                        Err(_) => {
                            // Timeout — check if block was already finalized
                            let current_height = block_blockchain.get_height();
                            if current_height >= block_height {
                                tracing::debug!(
                                    "✅ Block {} already finalized (chain at {}), skipping fallback",
                                    block_height,
                                    current_height
                                );
                                block_consensus_engine
                                    .timevote
                                    .cleanup_block_votes(block_hash);
                                // Skip rest of timeout handling
                            } else {
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
                                // Fallback only when we have votes from OTHER validators.
                                // The block producer always votes for its own block
                                // (prepare_weight >= 1), so require weight > 1 for networks
                                // with more than 2 validators. This prevents solo block
                                // production when no other node has confirmed the block.
                                //
                                // EXTENDED DEADLOCK OVERRIDE: after 15+ consecutive consensus
                                // timeouts (~150s) with no external votes, the other validators
                                // are likely offline or running old code that doesn't participate
                                // in BFT voting. At that point treat effective_validator_count as
                                // ≤2 so solo fallback is allowed.  This preserves safety for
                                // normal operation while restoring liveness when the voting
                                // quorum is permanently unavailable.
                                //
                                // CATCH-UP OVERRIDE: when far behind (>50 blocks), allow solo
                                // fallback immediately after the 2s consensus timeout. Votes from
                                // peers validating the same block are not reliable when many
                                // nodes are running old code and don't participate in BFT voting.
                                // The VRF leader election already proves this node was selected;
                                // near-tip safety checks (check_2_3_consensus_cached, blocks_behind≤10)
                                // still apply once the chain is close to expected height.
                                let effective_validator_count =
                                    if leader_attempt >= 15 || blocks_behind > 50 {
                                        validator_count.min(2)
                                    } else {
                                        validator_count
                                    };
                                let min_weight_for_fallback: u64 =
                                    if effective_validator_count <= 2 { 0 } else { 2 };
                                let should_fallback = prepare_weight >= min_weight_for_fallback
                                    && (prepare_weight > 1 || effective_validator_count <= 2);

                                if should_fallback {
                                    tracing::warn!(
                                    "⚡ Fallback: Adding block {} (prepare_weight={}, validators={})",
                                    block_height,
                                    prepare_weight,
                                    validator_count
                                );
                                    if let Err(e) = block_blockchain.add_block(block.clone()).await
                                    {
                                        tracing::error!(
                                            "❌ Failed to add block in fallback: {}",
                                            e
                                        );
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
                    }

                    // Check if we're still behind and need to continue immediately
                    let new_height = block_blockchain.get_height();
                    let new_expected = block_blockchain.calculate_expected_height();
                    let still_behind = new_expected.saturating_sub(new_height);
                    if still_behind > 0 {
                        // Invalidate consensus cache so next check uses fresh peer data
                        block_blockchain.invalidate_consensus_cache().await;

                        // Check how many peers are at our height before continuing.
                        // If slower peers have fallen behind, wait for them to catch up
                        // so the network stays in consensus during catch-up.
                        block_peer_registry
                            .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                            .await;

                        let connected_count =
                            block_peer_registry.get_connected_peers().await.len() as u32;
                        let two_thirds = (connected_count * 2 / 3).max(1);

                        let peers_at_our_height = {
                            let mut count = 0u32;
                            for peer_ip in &block_peer_registry.get_connected_peers().await {
                                if let Some((h, _)) =
                                    block_peer_registry.get_peer_chain_tip(peer_ip).await
                                {
                                    if h >= new_height {
                                        count += 1;
                                    }
                                }
                            }
                            count
                        };

                        if peers_at_our_height < two_thirds && blocks_behind <= 50 {
                            // Slower peers are behind — wait for 2/3 to catch up
                            let tip_signal = block_peer_registry.chain_tip_updated_signal();
                            let _ =
                                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                                    loop {
                                        tip_signal.notified().await;
                                        let mut count = 0u32;
                                        for peer_ip in
                                            &block_peer_registry.get_connected_peers().await
                                        {
                                            if let Some((h, _)) = block_peer_registry
                                                .get_peer_chain_tip(peer_ip)
                                                .await
                                            {
                                                if h >= new_height {
                                                    count += 1;
                                                }
                                            }
                                        }
                                        if count >= two_thirds {
                                            return;
                                        }
                                    }
                                })
                                .await;
                        }

                        tracing::debug!(
                            "🔄 Still {} blocks behind expected height {} — continuing",
                            still_behind,
                            new_expected
                        );

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

            // Pending and finalized transactions are never evicted by age.
            // A finalized TX must reach a block; a pending TX must reach consensus.
            // The re-broadcast loop re-relays both every 2 minutes until they land.

            tracing::debug!("🧹 Memory cleanup completed");
        }
    });
    shutdown_manager.register_task(cleanup_handle);

    // Re-broadcast orphaned finalized transactions.
    // When TXs are auto-finalized locally but peers miss the broadcast (e.g.,
    // no peers connected at finalization time), those TXs sit in the finalized
    // pool indefinitely — other block producers never include them. This task
    // re-sends TransactionFinalized every 2 minutes for any finalized TX older
    // than 60 seconds, ensuring peers eventually receive them.
    //
    // SAFETY: Before re-broadcasting, validates that each TX's input UTXOs are
    // still in SpentFinalized state. If inputs were restored (e.g., by
    // clearstucktransactions or UTXO reconciliation), the TX is evicted from
    // the finalized pool instead of re-broadcast, preventing infinite loops.
    let rebroadcast_consensus = consensus_engine.clone();
    let rebroadcast_peers = peer_connection_registry.clone();
    let rebroadcast_shutdown = shutdown_token.clone();
    let rebroadcast_handle = tokio::spawn(async move {
        const STALE_PENDING_REBROADCAST_AGE: std::time::Duration =
            std::time::Duration::from_secs(60);
        const STALE_PENDING_EVICTION_AGE: std::time::Duration =
            std::time::Duration::from_secs(24 * 60 * 60);

        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(120));
        interval.tick().await; // consume first immediate tick
        loop {
            tokio::select! {
                _ = rebroadcast_shutdown.cancelled() => {
                    tracing::debug!("🛑 Finalized TX re-broadcast task shutting down");
                    break;
                }
                _ = interval.tick() => {
                    let evicted_stale_pending = rebroadcast_consensus
                        .evict_stale_pending_transactions(STALE_PENDING_EVICTION_AGE)
                        .await;
                    if evicted_stale_pending > 0 {
                        tracing::warn!(
                            "🧹 Evicted {} stale pending transaction(s) older than one day",
                            evicted_stale_pending
                        );
                    }

                    let stale_finalized = rebroadcast_consensus
                        .get_stale_finalized(std::time::Duration::from_secs(60));
                    let stale_pending = rebroadcast_consensus
                        .tx_pool
                        .get_stale_pending(STALE_PENDING_REBROADCAST_AGE);

                    if stale_finalized.is_empty() && stale_pending.is_empty() {
                        continue;
                    }
                    let peer_count = rebroadcast_peers.connected_count();
                    if peer_count == 0 {
                        tracing::warn!(
                            "📡 {} orphaned finalized + {} orphaned pending TX(s) but no peers to re-broadcast to",
                            stale_finalized.len(), stale_pending.len()
                        );
                        continue;
                    }

                    // Re-broadcast pending transactions that are stuck waiting for TimeVote.
                    // These are transactions that peers may have missed (e.g. during a partition).
                    if !stale_pending.is_empty() {
                        tracing::info!(
                            "📡 Re-broadcasting {} orphaned pending TX(s) to {} peer(s)",
                            stale_pending.len(),
                            peer_count
                        );
                        for (_txid, tx) in stale_pending {
                            let msg = crate::network::message::NetworkMessage::TransactionBroadcast(tx);
                            rebroadcast_peers.broadcast(msg).await;
                        }
                    }

                    let stale = stale_finalized;
                    if stale.is_empty() {
                        continue;
                    }

                    // Validate each TX before re-broadcasting: ensure input UTXOs
                    // are still in a spent/locked state (not Unspent or Archived).
                    //
                    // IMPORTANT: after a daemon restart, input UTXOs for finalized-but-
                    // unarchived transactions will NOT be in utxo_states (they were removed
                    // from sled via mark_timevote_finalized and are only in the in-memory
                    // tombstone set). get_state() returns None for them, which is correct.
                    // Treat tombstoned + None as equivalent to SpentFinalized — the UTXO
                    // was legitimately spent by this TX and the tombstone proves it.
                    // Only evict when the input is genuinely Unspent (possible double-spend
                    // scenario, e.g. after a reindex) or Archived (already in a block).
                    let mut valid_txs = Vec::new();
                    let mut evict_txids = Vec::new();
                    for (txid, tx) in &stale {
                        let mut inputs_valid = true;
                        for input in &tx.inputs {
                            match rebroadcast_consensus.utxo_manager.get_state(&input.previous_output) {
                                Some(crate::types::UTXOState::SpentFinalized { .. }) => {}
                                Some(crate::types::UTXOState::SpentPending { .. }) => {}
                                Some(crate::types::UTXOState::Locked { .. }) => {}
                                None => {
                                    // None means not in state map.  If the outpoint is tombstoned
                                    // it was properly spent via finalization — keep the TX.
                                    // If it's genuinely missing (no record at all) it may have
                                    // been reverted; evict to prevent infinite re-broadcast loops.
                                    if !rebroadcast_consensus.utxo_manager.is_tombstoned(&input.previous_output) {
                                        tracing::warn!(
                                            "🧹 Evicting finalized TX {} from pool: input {} is missing from UTXO state and not tombstoned",
                                            hex::encode(txid),
                                            input.previous_output,
                                        );
                                        inputs_valid = false;
                                        break;
                                    }
                                    // else: tombstoned = legitimately spent, keep the TX
                                }
                                other => {
                                    // Unspent, Archived, or other unexpected state → evict
                                    tracing::warn!(
                                        "🧹 Evicting finalized TX {} from pool: input {} is {:?} (expected SpentFinalized/tombstoned)",
                                        hex::encode(txid),
                                        input.previous_output,
                                        other.as_ref().map(|s| format!("{}", s)).unwrap_or_else(|| "missing".to_string())
                                    );
                                    inputs_valid = false;
                                    break;
                                }
                            }
                        }
                        if inputs_valid {
                            valid_txs.push((*txid, tx.clone()));
                        } else {
                            evict_txids.push(*txid);
                        }
                    }

                    // Evict invalid TXs from the finalized pool
                    if !evict_txids.is_empty() {
                        tracing::warn!(
                            "🧹 Evicting {} finalized TX(s) with invalid input UTXOs",
                            evict_txids.len()
                        );
                        rebroadcast_consensus.clear_finalized_txs(&evict_txids);
                    }

                    if valid_txs.is_empty() {
                        continue;
                    }

                    tracing::info!(
                        "📡 Re-broadcasting {} orphaned finalized TX(s) to {} peer(s)",
                        valid_txs.len(),
                        peer_count
                    );
                    for (txid, tx) in valid_txs {
                        let msg = crate::network::message::NetworkMessage::TransactionFinalized {
                            txid,
                            tx,
                        };
                        rebroadcast_peers.broadcast(msg).await;
                    }
                }
            }
        }
    });
    shutdown_manager.register_task(rebroadcast_handle);

    // Periodic UTXO consistency check — hash-first, inquire on divergence
    // Runs at the midpoint between block slots (on the 5's: :05, :15, :25, etc.)
    // to avoid colliding with block production. Also runs when the network is
    // stalled (no new blocks) to help diagnose and recover from UTXO divergence.
    let utxo_sync_blockchain = blockchain_server.clone();
    let utxo_sync_peer_registry = peer_connection_registry.clone();
    let utxo_sync_shutdown = shutdown_token.clone();
    let utxo_sync_handle = tokio::spawn(async move {
        // Wait for initial sync to complete before checking consistency
        tokio::time::sleep(tokio::time::Duration::from_secs(120)).await;

        // Check every 30 seconds whether it's time for a UTXO consistency check.
        // The actual check only fires at block-slot midpoints (300s offset from genesis).
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_check_slot = 0i64;
        let mut last_stall_check = std::time::Instant::now();

        loop {
            tokio::select! {
                _ = utxo_sync_shutdown.cancelled() => {
                    tracing::debug!("🛑 UTXO consistency check shutting down gracefully");
                    break;
                }
                _ = interval.tick() => {
                    let genesis_ts = utxo_sync_blockchain.genesis_timestamp();
                    if genesis_ts == 0 {
                        continue; // No genesis yet
                    }

                    // Compute which 600s block slot we're in and how far into it
                    let now_ts = chrono::Utc::now().timestamp();
                    let elapsed = now_ts - genesis_ts;
                    let current_slot = elapsed / 600;
                    let offset_in_slot = elapsed % 600;

                    let our_height = utxo_sync_blockchain.get_height();
                    let expected_height = utxo_sync_blockchain.calculate_expected_height();
                    let is_stalled = expected_height > our_height + 3;

                    // Determine if we should fire:
                    // Normal mode: fire at midpoint (270-330s into slot) when in sync
                    // Stall mode: fire every 5 minutes regardless of slot timing
                    let should_fire = if is_stalled {
                        // Network is stalled — run every 5 minutes to help diagnose
                        if last_stall_check.elapsed() >= std::time::Duration::from_secs(300) {
                            last_stall_check = std::time::Instant::now();
                            true
                        } else {
                            false
                        }
                    } else {
                        // Normal mode: fire at the midpoint (270–330s into each slot)
                        if !(270..=330).contains(&offset_in_slot) {
                            continue;
                        }
                        if current_slot == last_check_slot {
                            continue;
                        }
                        last_check_slot = current_slot;
                        true
                    };

                    if !should_fire {
                        continue;
                    }

                    let connected = utxo_sync_peer_registry.get_connected_peers().await;
                    if connected.is_empty() {
                        continue;
                    }

                    let our_hash = utxo_sync_blockchain.get_utxo_state_hash().await;
                    let our_count = utxo_sync_blockchain.get_utxo_count().await;

                    tracing::info!(
                        "🔍 UTXO consistency check: broadcasting hash {} ({} UTXOs at height {}{}) to {} peer(s)",
                        hex::encode(&our_hash[..8]),
                        our_count,
                        our_height,
                        if is_stalled { " ⚠️ STALLED" } else { "" },
                        connected.len(),
                    );

                    utxo_sync_peer_registry
                        .broadcast(crate::network::message::NetworkMessage::GetUTXOStateHash)
                        .await;
                }
            }
        }
    });
    shutdown_manager.register_task(utxo_sync_handle);

    // Whitelist = time-coin.io discovered peers + operator-configured addnode peers.
    // addnode entries in time.conf are explicitly trusted by the operator; they bypass
    // AI cooldowns and the inbound redirect threshold just like official discovered peers.
    let mut combined_whitelist = discovered_peer_ips.clone();
    for peer in &config.network.bootstrap_peers {
        let ip = peer.split(':').next().unwrap_or(peer).to_string();
        if !combined_whitelist.contains(&ip) {
            combined_whitelist.push(ip);
        }
    }

    println!(
        "🔐 Preparing whitelist with {} trusted peer(s) from time-coin.io...",
        combined_whitelist.len()
    );
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
        config.network.blacklisted_subnets.clone(),
        combined_whitelist,
        network_type,
    )
    .await
    {
        Ok(mut server) => {
            // NOTE: Masternodes announced via P2P are NOT auto-whitelisted.
            // Only peers from time-coin.io DNS discovery are trusted at startup.

            // Wire up AI system for attack detection enforcement
            server.set_ai_system(ai_system.clone());

            // Enable separate attack log — one line per AI-detected attack event
            {
                let attack_log = Arc::new(crate::network::attack_log::AttackLog::new(
                    std::path::Path::new(&config.storage.data_dir),
                ));
                server.set_attack_log(attack_log);
                tracing::info!(
                    "🛡️ Attack log enabled: {}/attacks.log",
                    config.storage.data_dir
                );
            }

            // Enable blacklist persistence — bans now survive daemon restarts
            server
                .enable_blacklist_persistence(&blacklist_storage)
                .await;

            // Initialize TLS for encrypted P2P connections
            let tls_config = if config.security.enable_tls {
                match crate::network::tls::TlsConfig::new_self_signed() {
                    Ok(tls) => {
                        let tls = Arc::new(tls);
                        server.set_tls_config(tls.clone());
                        tracing::info!("🔒 TLS enabled for P2P connections");
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
                tracing::info!("⚠️ TLS disabled (tls=0 in config)");
                None
            };

            // Give registry access to network broadcast channel
            registry
                .set_broadcast_channel(server.tx_notifier.clone())
                .await;

            // Start gossip-based masternode status tracking
            registry.start_gossip_broadcaster(peer_connection_registry.clone());
            registry.start_report_cleanup(peer_connection_registry.clone());
            tracing::info!("✓ Gossip-based masternode status tracking started");

            // Start periodic reachability prober — identifies inbound-only nodes (behind NAT/
            // firewall) and excludes them from block rewards until bidirectional connectivity
            // is confirmed. Runs every 10 minutes after an initial 5-minute warm-up delay.
            MasternodeRegistry::start_reachability_prober(
                Arc::clone(&registry),
                peer_connection_registry.clone(),
            );
            tracing::info!("✓ Masternode reachability prober started (10-minute interval)");

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

            // Share blacklist with blockchain so sync_from_peers can skip banned peers
            blockchain.set_blacklist(server.blacklist.clone()).await;

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

            // Share WebSocket tx event sender with peer connection registry
            // so incoming network transactions trigger wallet notifications
            peer_connection_registry
                .set_tx_event_sender(tx_event_sender.clone())
                .await;

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
            let rpc_reconnection_ai = ai_system.reconnection_ai.clone();
            let rpc_user = config.rpc.rpcuser.clone();
            let rpc_pass = config.rpc.rpcpassword.clone();
            let rpc_auth_entries = config.rpc.rpcauth.clone();
            let rpc_tls_enabled = config.rpc.rpctls;
            let rpc_tls_cert = config.rpc.rpctlscert.clone();
            let rpc_tls_key = config.rpc.rpctlskey.clone();
            let rpc_data_dir = config.storage.data_dir.clone();
            let ws_tls_enabled = config.rpc.wstls;
            let ws_tls_cert = config.rpc.wstlscert.clone();
            let ws_tls_key = config.rpc.wstlskey.clone();
            let ws_data_dir = config.storage.data_dir.clone();

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
                    Some(rpc_reconnection_ai),
                    rpc_user,
                    rpc_pass,
                    rpc_auth_entries,
                )
                .await
                {
                    Ok(mut server) => {
                        // Set up TLS if configured
                        if rpc_tls_enabled {
                            use crate::network::tls::TlsConfig;
                            let tls_result = if !rpc_tls_cert.is_empty() && !rpc_tls_key.is_empty()
                            {
                                TlsConfig::from_pem_files(
                                    std::path::Path::new(&rpc_tls_cert),
                                    std::path::Path::new(&rpc_tls_key),
                                )
                            } else {
                                println!(
                                    "  🔐 No TLS cert/key specified, generating self-signed certificate"
                                );
                                // Save self-signed cert to data dir for CLI to trust
                                let result = TlsConfig::new_self_signed();
                                if result.is_ok() {
                                    let notice_path =
                                        std::path::Path::new(&rpc_data_dir).join("rpc_tls.txt");
                                    let _ = std::fs::write(
                                        &notice_path,
                                        "RPC TLS is enabled with a self-signed certificate.\n\
                                         Use --no-tls-verify with time-cli, or provide your own cert:\n\
                                         rpctlscert=/path/to/cert.pem\n\
                                         rpctlskey=/path/to/key.pem\n",
                                    );
                                }
                                result
                            };
                            match tls_result {
                                Ok(tls_config) => {
                                    server.set_tls(tls_config.acceptor());
                                    println!("  🔒 RPC TLS enabled");
                                }
                                Err(e) => {
                                    eprintln!(
                                        "  ⚠️  Failed to initialize RPC TLS: {}. Falling back to plain HTTP.",
                                        e
                                    );
                                }
                            }
                        } else {
                            println!("  ⚠️  RPC TLS disabled (rpctls=0 in config)");
                        }

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

            // Build WS TLS acceptor (enabled by default)
            let ws_tls_acceptor: Option<tokio_rustls::TlsAcceptor> = if ws_tls_enabled {
                use crate::network::tls::TlsConfig;
                let tls_result = if !ws_tls_cert.is_empty() && !ws_tls_key.is_empty() {
                    TlsConfig::from_pem_files(
                        std::path::Path::new(&ws_tls_cert),
                        std::path::Path::new(&ws_tls_key),
                    )
                } else {
                    TlsConfig::new_self_signed()
                };
                match tls_result {
                    Ok(tls_config) => {
                        let _ = std::fs::write(
                            std::path::Path::new(&ws_data_dir).join("ws_tls.txt"),
                            "WS TLS is enabled with a self-signed certificate.\n\
                             To disable: wstls=0 in time.conf\n\
                             To use your own cert: wstlscert=/path/cert.pem  wstlskey=/path/key.pem\n",
                        );
                        println!("  🔒 WebSocket TLS enabled");
                        Some(tls_config.acceptor())
                    }
                    Err(e) => {
                        eprintln!(
                            "  ⚠️  WebSocket TLS init failed: {}. Falling back to plain ws://.",
                            e
                        );
                        None
                    }
                }
            } else {
                None
            };

            let ws_handle = tokio::spawn(async move {
                if let Err(e) = rpc::websocket::start_ws_server(
                    &ws_addr,
                    ws_tx_sender,
                    ws_shutdown,
                    ws_tls_acceptor,
                )
                .await
                {
                    eprintln!("  ❌ WebSocket server error: {}", e);
                }
            });
            shutdown_manager.register_task(ws_handle);

            // Spawn a listener that emits WS notifications when transactions reach
            // consensus finality. This uses the ConsensusEngine's finalization signal
            // so wallets connected to ANY masternode get notified instantly.
            let finality_tx_pool = consensus_engine.tx_pool.clone();
            let finality_ws_sender = tx_event_sender.clone();
            let mut finality_rx = consensus_engine.subscribe_tx_finalized();
            let finality_shutdown = shutdown_token.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        result = finality_rx.recv() => {
                            match result {
                                Ok(txid) => {
                                    // Look up the transaction to get outputs
                                    if let Some(tx) = finality_tx_pool.get_transaction(&txid) {
                                        let outputs: Vec<rpc::websocket::TxOutputInfo> = tx
                                            .outputs
                                            .iter()
                                            .enumerate()
                                            .map(|(i, out)| {
                                                let address = String::from_utf8(out.script_pubkey.clone())
                                                    .unwrap_or_else(|_| hex::encode(&out.script_pubkey));
                                                rpc::websocket::TxOutputInfo {
                                                    address,
                                                    amount: out.value as f64 / 100_000_000.0,
                                                    index: i as u32,
                                                }
                                            })
                                            .collect();

                                        let event = rpc::websocket::TransactionEvent {
                                            txid: hex::encode(txid),
                                            outputs,
                                            timestamp: chrono::Utc::now().timestamp(),
                                            status: rpc::websocket::TxEventStatus::Finalized,
                                        };
                                        match finality_ws_sender.send(event) {
                                            Ok(n) => tracing::info!("📡 WS utxo_finalized sent to {} receiver(s)", n),
                                            Err(_) => tracing::warn!("📡 WS utxo_finalized failed: no receivers"),
                                        }
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::warn!("Finality WS notifier lagged by {} events", n);
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                        _ = finality_shutdown.cancelled() => break,
                    }
                }
            });

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
            // Share attack detector so outbound disconnects are tracked for AV3 detection
            network_client.set_attack_detector(ai_system.attack_detector.clone());
            if let Some(ref tls) = tls_config {
                network_client.set_tls_config(tls.clone());
            }
            network_client.set_discovered_peer_ips(discovered_peer_ips.clone());
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
                                // Rate-limit this warning (once per 60s)
                                static LAST_NO_PEERS_WARN: std::sync::atomic::AtomicI64 =
                                    std::sync::atomic::AtomicI64::new(0);
                                let now_secs = chrono::Utc::now().timestamp();
                                let last = LAST_NO_PEERS_WARN.load(std::sync::atomic::Ordering::Relaxed);
                                if now_secs - last >= 60 {
                                    LAST_NO_PEERS_WARN.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                                    tracing::warn!("⚠️ Bootstrap discovery: No connected peers found");
                                }
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

            // Persist the mempool so unconfirmed and finalized transactions survive the restart
            tracing::info!("💾 Persisting mempool to disk...");
            consensus_for_shutdown.save_mempool_to_sled(&block_storage_for_shutdown);

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

/// Build a MasternodeReg special transaction for the local masternode.
///
/// `wallet_key` is the key that owns the collateral UTXO (must produce an address matching
/// utxo.address). `operator_key` is the masternode node's hot key used for P2P identity;
/// pass `None` when both keys are the same (single-key setup).
///
/// Returns `None` if the masternode has no collateral outpoint (Free tier).
fn build_masternode_reg_tx(
    mn: &types::Masternode,
    wallet_key: &ed25519_dalek::SigningKey,
    _operator_key: Option<&ed25519_dalek::SigningKey>,
    p2p_port: u16,
) -> Option<types::Transaction> {
    use ed25519_dalek::Signer;

    let outpoint = mn.collateral_outpoint.as_ref()?;
    let outpoint_str = format!("{}:{}", hex::encode(outpoint.txid), outpoint.vout);
    let owner_pubkey = wallet_key.verifying_key();
    let owner_pubkey_hex = hex::encode(owner_pubkey.as_bytes());

    let message = {
        use sha2::{Digest, Sha256};
        let msg = format!(
            "MN_REG:{}:{}:{}:{}",
            outpoint_str, mn.address, p2p_port, mn.wallet_address
        );
        Sha256::digest(msg.as_bytes()).to_vec()
    };
    let signature = wallet_key.sign(&message);
    let signature_hex = hex::encode(signature.to_bytes());

    Some(types::Transaction {
        version: 1,
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        timestamp: chrono::Utc::now().timestamp(),
        special_data: Some(types::SpecialTransactionData::MasternodeRegistration {
            node_address: format!("{}:{}", mn.address, p2p_port),
            wallet_address: mn.wallet_address.clone(),
            reward_address: String::new(),
            collateral_outpoint: outpoint_str,
            pubkey: owner_pubkey_hex,
            signature: signature_hex,
        }),
        encrypted_memo: None,
    })
}

/// Build a MasternodeRegistration special transaction for a Free-tier node (no collateral).
///
/// Signed with `node_key`.
/// Message: `"MNREG:{node_address}:{wallet_address}:{pubkey}:none"`
fn build_free_node_reg_tx(
    node_address: &str,
    wallet_address: &str,
    node_key: &ed25519_dalek::SigningKey,
) -> Option<types::Transaction> {
    use ed25519_dalek::Signer;

    let pubkey_hex = hex::encode(node_key.verifying_key().as_bytes());
    let msg = format!(
        "MNREG:{}:{}:{}:none",
        node_address, wallet_address, pubkey_hex
    );
    let signature_hex = hex::encode(node_key.sign(msg.as_bytes()).to_bytes());

    Some(types::Transaction {
        version: 1,
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        timestamp: chrono::Utc::now().timestamp(),
        special_data: Some(types::SpecialTransactionData::MasternodeRegistration {
            node_address: node_address.to_string(),
            wallet_address: wallet_address.to_string(),
            reward_address: String::new(),
            collateral_outpoint: String::new(), // empty = Free tier
            pubkey: pubkey_hex,
            signature: signature_hex,
        }),
        encrypted_memo: None,
    })
}

/// Build a MasternodeDeregistration special transaction.
/// `slot_id` is the permanent on-chain slot assigned at registration.
/// Message: `"MNDEREG:{node_address}:{slot_id}"`
fn build_masternode_dereg_tx(
    node_address: &str,
    slot_id: u32,
    node_key: &ed25519_dalek::SigningKey,
) -> Option<types::Transaction> {
    use ed25519_dalek::Signer;

    let pubkey_hex = hex::encode(node_key.verifying_key().as_bytes());
    let msg = format!("MNDEREG:{}:{}", node_address, slot_id);
    let signature_hex = hex::encode(node_key.sign(msg.as_bytes()).to_bytes());

    Some(types::Transaction {
        version: 1,
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        timestamp: chrono::Utc::now().timestamp(),
        special_data: Some(types::SpecialTransactionData::MasternodeDeregistration {
            node_address: node_address.to_string(),
            slot_id,
            pubkey: pubkey_hex,
            signature: signature_hex,
        }),
        encrypted_memo: None,
    })
}

fn setup_logging(
    config: &config::LoggingConfig,
    verbose: bool,
    data_dir: &std::path::Path,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let level = if verbose { "trace" } else { &config.level };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{},sled=off", level)));

    // Detect if running under systemd/journald
    let is_systemd =
        std::env::var("JOURNAL_STREAM").is_ok() || std::env::var("INVOCATION_ID").is_ok();

    // Get hostname - shorten to first part before dot
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    let short_hostname = hostname.split('.').next().unwrap_or(&hostname).to_string();

    // Set up file appender writing to debug.log in the data directory
    std::fs::create_dir_all(data_dir).ok();

    // Rotate log if it exceeds 50 MB
    let log_path = data_dir.join("debug.log");
    let max_log_bytes: u64 = 50 * 1024 * 1024; // 50 MB
    if let Ok(meta) = std::fs::metadata(&log_path) {
        if meta.len() > max_log_bytes {
            let rotated = data_dir.join("debug.log.1");
            // Keep only one rotated copy — drop the older one if present
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(&log_path, &rotated);
        }
    }

    // Write a visible restart separator directly to the file before tracing takes over
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
            let _ = writeln!(f);
            let _ = writeln!(f);
            let _ = writeln!(f);
            let _ = writeln!(
                f,
                "# ═══════════════════════════════════════════════════════════════"
            );
            let _ = writeln!(f, "# NODE RESTART  {}", now);
            let _ = writeln!(
                f,
                "# ═══════════════════════════════════════════════════════════════"
            );
            let _ = writeln!(f);
        }
    }

    let file_appender = tracing_appender::rolling::never(data_dir, "debug.log");
    let (non_blocking_file, guard) = tracing_appender::non_blocking(file_appender);

    // File layer: plain text, no ANSI colors, always includes timestamp
    let file_layer = fmt::layer()
        .with_writer(non_blocking_file)
        .with_ansi(false)
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false)
        .with_timer(CustomTimer {
            hostname: short_hostname.clone(),
        });

    // Stdout layer: matches previous behavior (json / systemd-compact / pretty)
    match config.format.as_str() {
        "json" => {
            let stdout_layer = fmt::layer().json().with_thread_ids(false);

            tracing_subscriber::registry()
                .with(filter)
                .with(file_layer)
                .with(stdout_layer)
                .init();
        }
        _ => {
            if is_systemd {
                let stdout_layer = fmt::layer()
                    .with_target(false)
                    .with_thread_ids(false)
                    .with_thread_names(false)
                    .with_file(false)
                    .with_line_number(false)
                    .without_time()
                    .compact();

                tracing_subscriber::registry()
                    .with(filter)
                    .with(file_layer)
                    .with(stdout_layer)
                    .init();
            } else {
                let stdout_layer = fmt::layer()
                    .with_target(false)
                    .with_thread_ids(false)
                    .with_thread_names(false)
                    .with_file(false)
                    .with_line_number(false)
                    .with_timer(CustomTimer {
                        hostname: short_hostname,
                    })
                    .compact();

                tracing_subscriber::registry()
                    .with(filter)
                    .with(file_layer)
                    .with(stdout_layer)
                    .init();
            }
        }
    }

    Some(guard)
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
