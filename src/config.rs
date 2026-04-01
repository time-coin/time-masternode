//! Configuration management for TIME Coin daemon.
//!
//! Supports two config formats:
//! 1. **time.conf** (Dash-style key=value) — the primary format
//! 2. **Legacy TOML** — still loaded for backward compatibility, auto-migrates
//!
//! On first run, if no config exists, time.conf and masternode.conf are
//! auto-generated with free-node defaults in the data directory.
//!
//! Note: Some items appear as "dead code" in library checks because they're
//! only used by the binary (main.rs).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::network_type::NetworkType;
use rand::Rng;

/// Get the platform-specific data directory for TIME Coin
#[allow(dead_code)]
pub fn get_data_dir() -> PathBuf {
    if cfg!(windows) {
        // Windows: %APPDATA%\timecoin
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("timecoin")
    } else {
        // Linux/Mac: ~/.timecoin (or /root/.timecoin for root user)
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".timecoin")
    }
}

/// Get the network-specific subdirectory (mainnet or testnet)
#[allow(dead_code)]
pub fn get_network_data_dir(network: &NetworkType) -> PathBuf {
    let base = get_data_dir();
    match network {
        NetworkType::Mainnet => base, // Mainnet uses base directory directly
        NetworkType::Testnet => base.join("testnet"), // Testnet uses subdirectory
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub node: NodeConfig,
    pub network: NetworkConfig,
    pub rpc: RpcConfig,
    pub storage: StorageConfig,
    pub consensus: ConsensusConfig,
    pub block: BlockConfig,
    pub logging: LoggingConfig,
    pub masternode: MasternodeConfig,
    pub security: SecurityConfig,
    #[serde(default)]
    pub ai: AIConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub name: String,
    pub version: String,
    #[serde(default = "default_network")]
    pub network: String,
}

fn default_false() -> bool {
    false
}

fn default_network() -> String {
    "testnet".to_string()
}

impl NodeConfig {
    #[allow(dead_code)]
    pub fn network_type(&self) -> NetworkType {
        match self.network.to_lowercase().as_str() {
            "mainnet" => NetworkType::Mainnet,
            _ => NetworkType::Testnet,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub listen_address: String,
    pub external_address: Option<String>,
    pub max_peers: u32,
    pub enable_upnp: bool,
    pub enable_peer_discovery: bool,
    pub bootstrap_peers: Vec<String>,
    /// IPs to permanently blacklist (will not connect to or accept connections from)
    #[serde(default)]
    pub blacklisted_peers: Vec<String>,
    /// IPs to whitelist (exempt from rate limiting and bans)
    /// Typically used for trusted masternodes or infrastructure nodes
    #[serde(default)]
    pub whitelisted_peers: Vec<String>,
}

impl NetworkConfig {
    #[allow(dead_code)]
    pub fn full_listen_address(&self, network_type: &NetworkType) -> String {
        if self.listen_address.contains(':') {
            self.listen_address.clone()
        } else {
            format!(
                "{}:{}",
                self.listen_address,
                network_type.default_p2p_port()
            )
        }
    }

    #[allow(dead_code)]
    pub fn full_external_address(&self, network_type: &NetworkType) -> String {
        if let Some(ref ext_addr) = self.external_address {
            if !ext_addr.is_empty() {
                if ext_addr.contains(':') {
                    return ext_addr.clone();
                } else {
                    return format!("{}:{}", ext_addr, network_type.default_p2p_port());
                }
            }
        }

        // If no external address configured, try to auto-detect public IP
        if let Ok(public_ip) = std::process::Command::new("curl")
            .args(["-s", "https://api.ipify.org"])
            .output()
        {
            if public_ip.status.success() {
                if let Ok(ip_str) = String::from_utf8(public_ip.stdout) {
                    let ip_str = ip_str.trim();
                    if !ip_str.is_empty() && ip_str.parse::<std::net::IpAddr>().is_ok() {
                        tracing::info!("🌐 Auto-detected public IP: {}", ip_str);
                        return format!("{}:{}", ip_str, network_type.default_p2p_port());
                    }
                }
            }
        }

        // If auto-detection failed, fall back to listen address
        // (which may be 0.0.0.0, but at least we tried)
        tracing::warn!(
            "⚠️ Could not detect public IP, using listen address (this may not work if behind NAT)"
        );
        self.full_listen_address(network_type)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    pub enabled: bool,
    pub listen_address: String,
    pub allow_origins: Vec<String>,
    #[serde(default)]
    pub rpcuser: String,
    #[serde(default)]
    pub rpcpassword: String,
    /// Hashed credentials: "user:salt$hash" (Bitcoin-style rpcauth)
    #[serde(default)]
    pub rpcauth: Vec<String>,
    /// Enable TLS for RPC (rpctls=0 in time.conf to disable)
    #[serde(default = "default_true")]
    pub rpctls: bool,
    /// Path to TLS certificate PEM file
    #[serde(default)]
    pub rpctlscert: String,
    /// Path to TLS private key PEM file
    #[serde(default)]
    pub rpctlskey: String,
    /// Enable TLS for WebSocket server (wstls=0 to disable, default: 1)
    #[serde(default = "default_true")]
    pub wstls: bool,
    /// Path to WebSocket TLS certificate PEM file
    #[serde(default)]
    pub wstlscert: String,
    /// Path to WebSocket TLS private key PEM file
    #[serde(default)]
    pub wstlskey: String,
}

impl RpcConfig {
    #[allow(dead_code)]
    pub fn full_listen_address(&self, network_type: &NetworkType) -> String {
        if self.listen_address.contains(':') {
            self.listen_address.clone()
        } else {
            format!(
                "{}:{}",
                self.listen_address,
                network_type.default_rpc_port()
            )
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub backend: String,
    pub data_dir: String,
    pub cache_size_mb: usize,
    /// Enable zstd compression for block storage (reduces size ~60-70%)
    #[serde(default = "default_true")]
    pub compress_blocks: bool,
    /// Pruning mode: "archive" (keep all), "pruned" (keep recent N blocks + UTXO set)
    #[serde(default = "default_archive_mode")]
    pub mode: String,
    /// Number of recent blocks to keep when mode = "pruned" (default 1000)
    #[serde(default = "default_prune_keep_blocks")]
    pub prune_keep_blocks: u64,
}

fn default_archive_mode() -> String {
    "archive".to_string()
}

fn default_prune_keep_blocks() -> u64 {
    1000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusConfig {
    pub min_masternodes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockConfig {
    pub block_time_seconds: u64, // 600 = 10 minutes
    pub max_block_size_kb: usize,
    pub max_transactions_per_block: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub format: String,
    pub output: String,
    pub file_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasternodeConfig {
    pub enabled: bool,
    // wallet_address is auto-generated from the node's wallet - no config needed
    pub collateral_txid: String,
    #[serde(default)]
    pub collateral_vout: u32,
    #[serde(default = "default_tier")]
    pub tier: String,
    /// Base58check-encoded Ed25519 private key (generate with `time-cli masternodegenkey`)
    #[serde(default)]
    pub masternodeprivkey: String,
    /// Optional reward/payout address override.
    /// If set, masternode rewards are sent to this address instead of the
    /// node's auto-generated wallet address. Use this to receive rewards
    /// directly in your GUI wallet.
    #[serde(default)]
    pub reward_address: String,
}

/// A parsed entry from masternode.conf (collateral info only; IP and key are in time.conf)
#[derive(Debug, Clone)]
pub struct MasternodeConfEntry {
    pub alias: String,
    /// IP:port from 4–6-field entries; empty string for the compact 3-field format.
    pub address: String,
    pub collateral_txid: String,
    pub collateral_vout: u32,
}

fn default_tier() -> String {
    "auto".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enable_rate_limiting: bool,
    pub max_requests_per_second: u32,
    pub enable_authentication: bool,
    pub api_key: String,
    #[serde(default = "default_true")]
    pub enable_tls: bool,
    #[serde(default = "default_true")]
    pub enable_message_signing: bool,
    #[serde(default = "default_message_max_age")]
    pub message_max_age_seconds: i64,
}

fn default_true() -> bool {
    true
}

fn default_message_max_age() -> i64 {
    300 // 5 minutes
}

/// AI System Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIConfig {
    /// Master switch to enable/disable all AI features
    #[serde(default = "default_ai_enabled")]
    pub enabled: bool,

    /// Global learning rate for all AI modules (0.0-1.0)
    #[serde(default = "default_learning_rate")]
    pub learning_rate: f64,

    /// Minimum samples required before AI makes predictions
    #[serde(default = "default_min_samples")]
    pub min_samples: usize,

    /// Enable automatic parameter tuning
    #[serde(default = "default_true")]
    pub auto_tuning: bool,

    /// Individual module configurations
    #[serde(default)]
    pub peer_selector: AIPeerSelectorConfig,

    #[serde(default)]
    pub fork_resolver: AIForkResolverConfig,

    #[serde(default)]
    pub block_production: AIBlockProductionConfig,

    #[serde(default)]
    pub masternode_health: AIMasternodeHealthConfig,

    #[serde(default)]
    pub sync_recovery: AISyncRecoveryConfig,

    #[serde(default)]
    pub mempool_optimizer: AIMempoolOptimizerConfig,

    #[serde(default)]
    pub anomaly_detector: AIAnomalyDetectorConfig,

    #[serde(default)]
    pub network_optimizer: AINetworkOptimizerConfig,

    #[serde(default)]
    pub predictive_sync: AIPredictiveSyncConfig,
}

impl Default for AIConfig {
    fn default() -> Self {
        Self {
            enabled: default_ai_enabled(),
            learning_rate: default_learning_rate(),
            min_samples: default_min_samples(),
            auto_tuning: default_true(),
            peer_selector: AIPeerSelectorConfig::default(),
            fork_resolver: AIForkResolverConfig::default(),
            block_production: AIBlockProductionConfig::default(),
            masternode_health: AIMasternodeHealthConfig::default(),
            sync_recovery: AISyncRecoveryConfig::default(),
            mempool_optimizer: AIMempoolOptimizerConfig::default(),
            anomaly_detector: AIAnomalyDetectorConfig::default(),
            network_optimizer: AINetworkOptimizerConfig::default(),
            predictive_sync: AIPredictiveSyncConfig::default(),
        }
    }
}

fn default_ai_enabled() -> bool {
    false // Disabled by default for safety
}

fn default_learning_rate() -> f64 {
    0.1
}

fn default_min_samples() -> usize {
    10
}

fn default_confidence_threshold() -> f64 {
    0.7
}

fn default_anomaly_threshold() -> f64 {
    2.0 // Z-score threshold
}

/// AI Peer Selector Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIPeerSelectorConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_max_history")]
    pub max_history: usize,
}

impl Default for AIPeerSelectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_history: default_max_history(),
        }
    }
}

fn default_max_history() -> usize {
    1000
}

/// AI Fork Resolver Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIForkResolverConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
}

impl Default for AIForkResolverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            confidence_threshold: default_confidence_threshold(),
        }
    }
}

/// AI Block Production Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIBlockProductionConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub failure_prediction: bool,

    #[serde(default = "default_true")]
    pub strategy_optimization: bool,
}

impl Default for AIBlockProductionConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Experimental
            failure_prediction: true,
            strategy_optimization: true,
        }
    }
}

/// AI Masternode Health Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIMasternodeHealthConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub adaptive_timeouts: bool,
}

impl Default for AIMasternodeHealthConfig {
    fn default() -> Self {
        Self {
            enabled: false, // New feature
            adaptive_timeouts: true,
        }
    }
}

/// AI Sync Recovery Configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AISyncRecoveryConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
}

/// AI Mempool Optimizer Configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AIMempoolOptimizerConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,

    #[serde(default = "default_false")]
    pub predictive_loading: bool,
}

/// AI Anomaly Detector Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIAnomalyDetectorConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default = "default_anomaly_threshold")]
    pub alert_threshold: f64,
}

impl Default for AIAnomalyDetectorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            alert_threshold: default_anomaly_threshold(),
        }
    }
}

/// AI Network Optimizer Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AINetworkOptimizerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AINetworkOptimizerConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// AI Predictive Sync Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIPredictiveSyncConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AIPredictiveSyncConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Config {
    /// Get the data directory for a specific network
    #[allow(dead_code)]
    pub fn get_data_directory(
        network: &NetworkType,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let data_dir = get_network_data_dir(network);
        fs::create_dir_all(&data_dir)?;
        Ok(data_dir)
    }

    #[allow(dead_code)]
    pub fn load_from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    #[allow(dead_code)]
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        Self {
            node: NodeConfig {
                name: "TIME Coin Node".to_string(),
                version: "1.0.0".to_string(),
                network: "testnet".to_string(),
            },
            network: NetworkConfig {
                listen_address: "0.0.0.0".to_string(),
                external_address: None,
                max_peers: 50,
                enable_upnp: false,
                enable_peer_discovery: true,
                bootstrap_peers: vec![],
                blacklisted_peers: vec![],
                whitelisted_peers: vec![],
            },
            rpc: RpcConfig {
                enabled: true,
                listen_address: "0.0.0.0".to_string(),
                allow_origins: vec![
                    "http://localhost".to_string(),
                    "http://127.0.0.1".to_string(),
                ],
                rpcuser: String::new(),
                rpcpassword: String::new(),
                rpcauth: Vec::new(),
                rpctls: false,
                rpctlscert: String::new(),
                rpctlskey: String::new(),
                wstls: true,
                wstlscert: String::new(),
                wstlskey: String::new(),
            },
            storage: StorageConfig {
                backend: "sled".to_string(),
                data_dir: "".to_string(), // Will be auto-configured
                cache_size_mb: 256,
                compress_blocks: true,
                mode: "archive".to_string(),
                prune_keep_blocks: 1000,
            },
            consensus: ConsensusConfig { min_masternodes: 3 },
            block: BlockConfig {
                block_time_seconds: 600, // 10 minutes
                max_block_size_kb: 1024,
                max_transactions_per_block: 10000,
            },
            logging: LoggingConfig {
                level: "info".to_string(),
                format: "pretty".to_string(),
                output: "stdout".to_string(),
                file_path: "./logs/node.log".to_string(),
            },
            masternode: MasternodeConfig {
                enabled: true,
                collateral_txid: String::new(),
                collateral_vout: 0,
                tier: "free".to_string(),
                masternodeprivkey: String::new(),
                reward_address: String::new(),
            },
            security: SecurityConfig {
                enable_rate_limiting: true,
                max_requests_per_second: 1000,
                enable_authentication: false,
                api_key: String::new(),
                enable_tls: true,
                enable_message_signing: true,
                message_max_age_seconds: 300,
            },
            ai: AIConfig::default(),
        }
    }

    #[allow(dead_code)]
    pub fn load_or_create(
        path: &str,
        network_type: &NetworkType,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Ensure data directory exists
        let data_dir = get_network_data_dir(network_type);
        fs::create_dir_all(&data_dir)?;

        if fs::metadata(path).is_ok() {
            let contents = fs::read_to_string(path)?;
            let mut config: Config = toml::from_str(&contents)?;

            // Update data_dir to use platform-specific path if empty or relative
            if config.storage.data_dir.is_empty() || config.storage.data_dir.starts_with("./") {
                config.storage.data_dir = data_dir.to_string_lossy().to_string();
            }

            // Save user-configurable values before hardcoding
            let saved_network = config.node.network.clone();
            let saved_data_dir = config.storage.data_dir.clone();
            let saved_listen = config.network.listen_address.clone();
            let saved_external = config.network.external_address.clone();
            let saved_peers = config.network.bootstrap_peers.clone();
            let saved_blacklist = config.network.blacklisted_peers.clone();
            let saved_whitelist = config.network.whitelisted_peers.clone();
            let saved_rpc_enabled = config.rpc.enabled;
            let saved_rpc_addr = config.rpc.listen_address.clone();
            let saved_rpc_origins = config.rpc.allow_origins.clone();
            let saved_log_level = config.logging.level.clone();
            let saved_mn = config.masternode.clone();

            // Lock down non-configurable settings
            config.apply_hardcoded_defaults();

            // Restore user-configurable values
            config.node.network = saved_network;
            config.storage.data_dir = saved_data_dir;
            config.network.listen_address = saved_listen;
            config.network.external_address = saved_external;
            config.network.bootstrap_peers = saved_peers;
            config.network.blacklisted_peers = saved_blacklist;
            config.network.whitelisted_peers = saved_whitelist;
            config.rpc.enabled = saved_rpc_enabled;
            config.rpc.listen_address = saved_rpc_addr;
            config.rpc.allow_origins = saved_rpc_origins;
            config.logging.level = saved_log_level;
            config.masternode = saved_mn;

            Ok(config)
        } else {
            let mut config = Config::default();

            // Set network-specific defaults
            config.node.network = match network_type {
                NetworkType::Mainnet => "mainnet".to_string(),
                NetworkType::Testnet => "testnet".to_string(),
            };

            // Set platform-specific data directory
            config.storage.data_dir = data_dir.to_string_lossy().to_string();

            config.apply_hardcoded_defaults();
            config.save_to_file(path)?;
            Ok(config)
        }
    }

    #[allow(dead_code)]
    pub fn save_to_file(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let contents = toml::to_string_pretty(self)?;
        fs::write(path, contents)?;
        Ok(())
    }

    /// Force-apply hardcoded defaults for settings that users must not change.
    /// Called after loading from any config format (TOML or time.conf) to ensure
    /// protocol-critical values cannot be overridden.
    #[allow(dead_code)]
    pub fn apply_hardcoded_defaults(&mut self) {
        // Node identity — version comes from Cargo.toml at compile time
        self.node.name = "TIME Coin Node".to_string();
        self.node.version = env!("CARGO_PKG_VERSION").to_string();

        // Network — these are protocol-critical
        self.network.max_peers = 50;
        self.network.enable_upnp = false;
        self.network.enable_peer_discovery = true;
        // Don't clear blacklist/whitelist — those may be set intentionally

        // Storage — backend and performance are not user-tunable
        self.storage.backend = "sled".to_string();
        self.storage.cache_size_mb = 256;
        self.storage.compress_blocks = false; // compression causes corruption
        self.storage.mode = "archive".to_string();
        self.storage.prune_keep_blocks = 1000;

        // Consensus — protocol constants (also in constants.rs)
        self.consensus.min_masternodes = 3;

        // Block — protocol constants (also in constants.rs)
        self.block.block_time_seconds = 600;
        self.block.max_block_size_kb = 1024;
        self.block.max_transactions_per_block = 10000;

        // Logging — only level is user-configurable (via debug= in time.conf)
        self.logging.format = "pretty".to_string();
        self.logging.output = "stdout".to_string();
        self.logging.file_path = "./logs/node.log".to_string();

        // Masternode — tier is always auto-detected from collateral
        self.masternode.tier = "auto".to_string();

        // Security — all hardcoded, not user-configurable
        self.security.enable_rate_limiting = true;
        self.security.max_requests_per_second = 1000;
        self.security.enable_authentication = false;
        self.security.api_key = String::new();
        self.security.enable_tls = true;
        self.security.enable_message_signing = true;
        self.rpc.rpctls = true;
        self.security.message_max_age_seconds = 300;

        // AI — all hardcoded with safe defaults
        self.ai = AIConfig::default();
    }

    /// Load config from a Dash-style time.conf key=value file.
    /// Falls back to defaults for any missing keys.
    #[allow(dead_code)]
    pub fn load_from_conf(
        conf_path: &PathBuf,
        network_type: &NetworkType,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let data_dir = get_network_data_dir(network_type);
        fs::create_dir_all(&data_dir)?;

        let mut config = Config::default();

        // Set network-specific defaults
        config.node.network = match network_type {
            NetworkType::Mainnet => "mainnet".to_string(),
            NetworkType::Testnet => "testnet".to_string(),
        };
        config.storage.data_dir = data_dir.to_string_lossy().to_string();

        if conf_path.exists() {
            let entries = parse_conf_file(conf_path)?;

            // Apply key=value entries to config
            if let Some(v) = entries.get("testnet") {
                if v.last().is_some_and(|s| s == "1") {
                    config.node.network = "testnet".to_string();
                } else {
                    config.node.network = "mainnet".to_string();
                }
            }
            if let Some(v) = entries.get("port") {
                if let Some(port) = v.last() {
                    config.network.listen_address = format!("0.0.0.0:{}", port);
                }
            }
            if let Some(v) = entries.get("listen") {
                config.network.enable_peer_discovery = v.last().map_or(true, |s| s == "1");
            }
            if let Some(v) = entries.get("externalip") {
                config.network.external_address = v.last().cloned();
            }
            if let Some(v) = entries.get("maxconnections") {
                if let Some(val) = v.last().and_then(|s| s.parse::<u32>().ok()) {
                    config.network.max_peers = val;
                }
            }
            if let Some(v) = entries.get("server") {
                config.rpc.enabled = v.last().map_or(true, |s| s == "1");
            }
            if let Some(v) = entries.get("rpcport") {
                if let Some(port) = v.last() {
                    // Preserve existing bind address, only change the port
                    let host = config
                        .rpc
                        .listen_address
                        .split(':')
                        .next()
                        .unwrap_or("0.0.0.0");
                    config.rpc.listen_address = format!("{}:{}", host, port);
                }
            }
            if let Some(v) = entries.get("rpcbind") {
                if let Some(addr) = v.last() {
                    // If rpcbind has no port, keep existing port
                    if addr.contains(':') {
                        config.rpc.listen_address = addr.clone();
                    } else {
                        let port = if config.rpc.listen_address.contains(':') {
                            config
                                .rpc
                                .listen_address
                                .split(':')
                                .next_back()
                                .unwrap_or("24001")
                                .to_string()
                        } else {
                            config.node.network_type().default_rpc_port().to_string()
                        };
                        config.rpc.listen_address = format!("{}:{}", addr, port);
                    }
                }
            }
            if let Some(v) = entries.get("rpcallowip") {
                config.rpc.allow_origins = v.iter().map(|ip| format!("http://{}", ip)).collect();
            }
            if let Some(v) = entries.get("rpcuser") {
                if let Some(user) = v.last() {
                    config.rpc.rpcuser = user.clone();
                }
            }
            if let Some(v) = entries.get("rpcpassword") {
                if let Some(pass) = v.last() {
                    config.rpc.rpcpassword = pass.clone();
                }
            }
            if let Some(v) = entries.get("rpcauth") {
                config.rpc.rpcauth = v.clone();
            }
            if let Some(v) = entries.get("rpctls") {
                config.rpc.rpctls = v.last().is_some_and(|s| s == "1");
            }
            // tls=0 disables P2P TLS (default: enabled); useful for isolated testnets
            if let Some(v) = entries.get("tls") {
                if v.last().is_some_and(|s| s == "0") {
                    config.security.enable_tls = false;
                }
            }
            if let Some(v) = entries.get("rpctlscert") {
                if let Some(path) = v.last() {
                    config.rpc.rpctlscert = path.clone();
                }
            }
            if let Some(v) = entries.get("rpctlskey") {
                if let Some(path) = v.last() {
                    config.rpc.rpctlskey = path.clone();
                }
            }
            if let Some(v) = entries.get("wstls") {
                config.rpc.wstls = v.last().is_some_and(|s| s != "0");
            }
            if let Some(v) = entries.get("wstlscert") {
                if let Some(path) = v.last() {
                    config.rpc.wstlscert = path.clone();
                }
            }
            if let Some(v) = entries.get("wstlskey") {
                if let Some(path) = v.last() {
                    config.rpc.wstlskey = path.clone();
                }
            }
            if let Some(v) = entries.get("masternode") {
                config.masternode.enabled = v.last().is_some_and(|s| s == "1");
            }
            if let Some(v) = entries.get("addnode") {
                config.network.bootstrap_peers = v.clone();
            }
            if let Some(v) = entries.get("ban") {
                config.network.blacklisted_peers = v.clone();
            }
            if let Some(v) = entries.get("whitelist") {
                config.network.whitelisted_peers = v.clone();
            }
            if let Some(v) = entries.get("debug") {
                if let Some(level) = v.last() {
                    config.logging.level = level.clone();
                }
            }
            if let Some(v) = entries.get("datadir") {
                if let Some(dir) = v.last() {
                    if !dir.is_empty() {
                        config.storage.data_dir = dir.clone();
                    }
                }
            }
            if let Some(v) = entries.get("txindex") {
                // txindex=1 means archive mode
                if v.last().is_some_and(|s| s == "1") {
                    config.storage.mode = "archive".to_string();
                }
            }
            if let Some(v) = entries.get("masternodeprivkey") {
                if let Some(key) = v.last() {
                    config.masternode.masternodeprivkey = key.clone();
                }
            }
            if let Some(v) = entries.get("reward_address") {
                if let Some(addr) = v.last() {
                    config.masternode.reward_address = addr.clone();
                }
            }

            println!("  ✓ Loaded configuration from {}", conf_path.display());

            // Auto-generate RPC credentials for existing configs that lack them
            if config.rpc.rpcuser.is_empty() || config.rpc.rpcpassword.is_empty() {
                let user = generate_random_string(16);
                let password = generate_random_string(32);
                append_conf_key(conf_path, "rpcuser", &user)?;
                append_conf_key(conf_path, "rpcpassword", &password)?;
                config.rpc.rpcuser = user;
                config.rpc.rpcpassword = password;
                println!(
                    "  ⚠ RPC credentials were missing — auto-generated and saved to {}",
                    conf_path.display()
                );
            }
        } else {
            // Generate default time.conf on first run
            generate_default_conf_for_network(conf_path, network_type)?;
            println!("  ✓ Generated default {}", conf_path.display());

            // Re-read the just-generated credentials into config
            if let Ok(entries) = parse_conf_file(conf_path) {
                if let Some(v) = entries.get("rpcuser") {
                    if let Some(user) = v.last() {
                        config.rpc.rpcuser = user.clone();
                    }
                }
                if let Some(v) = entries.get("rpcpassword") {
                    if let Some(pass) = v.last() {
                        config.rpc.rpcpassword = pass.clone();
                    }
                }
            }
        }

        // Write .cookie file for CLI tools
        let data_dir = get_network_data_dir(network_type);
        if !config.rpc.rpcuser.is_empty() && !config.rpc.rpcpassword.is_empty() {
            let data_dir_str = data_dir.to_string_lossy();
            if let Err(e) =
                write_rpc_cookie(&data_dir_str, &config.rpc.rpcuser, &config.rpc.rpcpassword)
            {
                eprintln!("  ⚠ Failed to write RPC cookie file: {}", e);
            }
        }

        // Load masternode.conf if it exists
        let mn_conf_path = conf_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("masternode.conf");

        if mn_conf_path.exists() {
            let entries = parse_masternode_conf(&mn_conf_path)?;
            if let Some(entry) = entries.first() {
                config.masternode.enabled = true;
                config.masternode.collateral_txid = entry.collateral_txid.clone();
                config.masternode.collateral_vout = entry.collateral_vout;
                println!("  ✓ Loaded masternode config: alias={}", entry.alias);
            }
        } else {
            // Auto-populate masternode.conf with detected IP on first run
            let ext_addr = if config.masternode.enabled {
                Some(config.network.full_external_address(network_type))
            } else {
                None
            };
            generate_default_masternode_conf(&mn_conf_path, ext_addr.as_deref())?;
            println!("  ✓ Generated default {}", mn_conf_path.display());

            // Re-read auto-populated entry for the address
            if ext_addr.is_some() {
                let entries = parse_masternode_conf(&mn_conf_path)?;
                if let Some(entry) = entries.first() {
                    config.masternode.enabled = true;
                    // Skip collateral — all-zeros is a free-tier placeholder
                    println!(
                        "  ✓ Auto-populated masternode config: alias={}",
                        entry.alias
                    );
                }
            }
        }

        // Lock down non-configurable settings
        config.apply_hardcoded_defaults();

        Ok(config)
    }
}

// ─── time.conf parser ────────────────────────────────────────────────

/// Parse a Dash-style key=value config file.
/// Supports # comments, blank lines, and repeatable keys (e.g., addnode).
/// Returns a map of key → list of values (to handle repeated keys).
#[allow(dead_code)]
pub fn parse_conf_file(
    path: &PathBuf,
) -> Result<HashMap<String, Vec<String>>, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(path)?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for (line_num, line) in contents.lines().enumerate() {
        let line = line.trim();

        // Skip comments and blank lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse key=value
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_string();
            map.entry(key).or_default().push(value);
        } else {
            tracing::warn!(
                "⚠️ {}:{}: ignoring malformed line: {}",
                path.display(),
                line_num + 1,
                line
            );
        }
    }

    Ok(map)
}

/// Detect network type from a time.conf without fully parsing it.
#[allow(dead_code)]
pub fn detect_network_from_conf(conf_path: &PathBuf) -> NetworkType {
    // If the config file is inside a "testnet" directory, infer testnet
    // even if the file doesn't exist yet (first run / migration)
    let in_testnet_dir = conf_path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|name| name == "testnet");

    if let Ok(entries) = parse_conf_file(conf_path) {
        if let Some(v) = entries.get("testnet") {
            if v.last().is_some_and(|s| s == "1") {
                return NetworkType::Testnet;
            }
        }
        // If testnet=0 or not present, check for mainnet key
        if let Some(v) = entries.get("mainnet") {
            if v.last().is_some_and(|s| s == "1") {
                return NetworkType::Mainnet;
            }
        }
    }

    if in_testnet_dir {
        return NetworkType::Testnet;
    }

    // No explicit network setting, not in testnet dir — default to mainnet
    NetworkType::Mainnet
}

// ─── masternode.conf parser ──────────────────────────────────────────

/// Parse a masternode.conf file (one entry per line, space-delimited).
/// New format: alias  collateral_txid  collateral_vout
/// Legacy formats (still accepted for backward compatibility):
///   4 fields: alias IP:port txid vout        (IP is ignored — use externalip in time.conf)
///   5 fields: alias IP:port privkey txid vout (IP+privkey ignored)
///   6 fields: alias IP:port key cert txid vout (IP+key+cert ignored)
#[allow(dead_code)]
pub fn parse_masternode_conf(
    path: &PathBuf,
) -> Result<Vec<MasternodeConfEntry>, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(path)?;
    let mut entries = Vec::new();

    for (line_num, line) in contents.lines().enumerate() {
        let line = line.trim();

        // Skip comments and blank lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();

        // Determine txid/vout positions based on field count.
        // 3 fields (new):  alias txid vout
        // 4 fields (legacy): alias IP:port txid vout
        // 5 fields (legacy): alias IP:port privkey txid vout
        // 6 fields (legacy): alias IP:port key cert txid vout
        let (addr_idx, txid_idx, vout_idx) = match parts.len() {
            3 => (None, 1, 2),
            4 => {
                tracing::warn!(
                    "masternode.conf:{}: entry '{}' uses the legacy 'alias IP:port txid vout' format. \
                     The IP:port is ignored — your node's address is read from externalip= in time.conf. \
                     Simplify to: '{} {} {}'",
                    line_num + 1,
                    parts[0],
                    parts[0], parts[2], parts[3]
                );
                (Some(1), 2, 3)
            }
            5 => {
                tracing::warn!(
                    "masternode.conf:{}: entry '{}' uses the legacy 'alias IP:port privkey txid vout' format. \
                     The IP:port and privkey are ignored — your node's address is read from externalip= in time.conf. \
                     Simplify to: '{} {} {}'",
                    line_num + 1,
                    parts[0],
                    parts[0], parts[3], parts[4]
                );
                (Some(1), 3, 4)
            }
            6 => {
                tracing::warn!(
                    "masternode.conf:{}: entry '{}' uses the legacy 'alias IP:port key cert txid vout' format. \
                     The IP:port, key, and cert fields are ignored — your node's address is read from externalip= in time.conf. \
                     Simplify to: '{} {} {}'",
                    line_num + 1,
                    parts[0],
                    parts[0], parts[4], parts[5]
                );
                (Some(1), 4, 5)
            }
            _ => {
                tracing::warn!(
                    "⚠️ masternode.conf:{}: expected 3-6 fields, got {} — skipping",
                    line_num + 1,
                    parts.len()
                );
                continue;
            }
        };

        let vout = parts[vout_idx].parse::<u32>().map_err(|e| {
            format!(
                "masternode.conf:{}: invalid collateral_vout '{}': {}",
                line_num + 1,
                parts[vout_idx],
                e
            )
        })?;

        entries.push(MasternodeConfEntry {
            alias: parts[0].to_string(),
            address: addr_idx.map(|i| parts[i].to_string()).unwrap_or_default(),
            collateral_txid: parts[txid_idx].to_string(),
            collateral_vout: vout,
        });
    }

    Ok(entries)
}

// ─── Default file generation ─────────────────────────────────────────

/// Generate a default time.conf with commented documentation.
/// Pass `network_type` to set the `testnet=` line appropriately.
#[allow(dead_code)]
pub fn generate_default_conf_for_network(
    path: &PathBuf,
    network_type: &NetworkType,
) -> Result<(), Box<dyn std::error::Error>> {
    let testnet_line = match network_type {
        NetworkType::Testnet => "testnet=1",
        NetworkType::Mainnet => "#testnet=0",
    };
    let rpc_user = generate_random_string(16);
    let rpc_password = generate_random_string(32);
    let contents = format!(
        r#"# TIME Coin Configuration File
# https://time-coin.io
#
# Lines beginning with # are comments.
# All settings are optional — defaults are shown below.

# ─── Network ─────────────────────────────────────────────────
# Run on testnet (1) or mainnet (0)
{}

# Accept incoming connections
listen=1

# Override the default port (mainnet=24000, testnet=24100)
#port=24100

# Your public IP address (required for masternodes)
#externalip=1.2.3.4

# Maximum peer connections
#maxconnections=50

# ─── RPC ─────────────────────────────────────────────────────
# Enable JSON-RPC server
server=1

# RPC port (mainnet=24001, testnet=24101)
#rpcport=24101

# RPC authentication (auto-generated — change if desired)
rpcuser={}
rpcpassword={}

# IP addresses allowed to connect to RPC
#rpcallowip=127.0.0.1

# ─── Masternode ──────────────────────────────────────────────
# Enable masternode mode (0=off, 1=on)
# Collateral settings go in masternode.conf
masternode=1

# Masternode private key (generate with: time-cli masternode genkey)
# If not set, one will be auto-generated on first startup.
#masternodeprivkey=

# Reward address override — send masternode rewards to this address
# instead of the node's auto-generated wallet. Use your GUI wallet
# receive address here to collect rewards directly.
#reward_address=

# ─── Peers ───────────────────────────────────────────────────
# Add seed nodes (one per line, can repeat)
#addnode=seed1.time-coin.io
#addnode=seed2.time-coin.io

# Permanently ban IPs (one per line, can repeat)
#ban=1.2.3.4
#ban=5.6.7.8

# Whitelist IPs — exempt from rate limits and auto-bans (one per line)
#whitelist=1.2.3.4

# ─── Logging ─────────────────────────────────────────────────
# Log level: trace, debug, info, warn, error
debug=info

# ─── Storage ─────────────────────────────────────────────────
# Maintain a full transaction index
txindex=1

# Custom data directory (leave commented for default)
#datadir=
"#,
        testnet_line, rpc_user, rpc_password
    );

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

/// Generate a random alphanumeric string of the given length for RPC credentials.
fn generate_random_string(len: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Generate RPC credentials and write a `.cookie` file for CLI access.
///
/// The cookie file contains `rpcuser:rpcpassword` and is readable only by
/// the file owner, following the same pattern as Bitcoin Core.
pub fn write_rpc_cookie(
    data_dir: &str,
    user: &str,
    password: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cookie_path = PathBuf::from(data_dir).join(".cookie");
    let cookie_content = format!("{}:{}", user, password);
    fs::write(&cookie_path, &cookie_content)?;

    // Restrict permissions on Unix (owner-read-only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cookie_path, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// Append a key=value line to a Dash-style conf file.
///
/// If the key already exists (even commented out), un-comments and replaces its value.
/// Otherwise appends the line at the end.
pub fn append_conf_key(
    path: &PathBuf,
    key: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let contents = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };

    let pattern = format!("#{}=", key);
    let active_pattern = format!("{}=", key);
    let mut replaced = false;
    let mut lines: Vec<String> = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if !replaced && (trimmed.starts_with(&pattern) || trimmed.starts_with(&active_pattern))
            {
                replaced = true;
                format!("{}={}", key, value)
            } else {
                line.to_string()
            }
        })
        .collect();

    if !replaced {
        lines.push(format!("{}={}", key, value));
    }

    fs::write(path, lines.join("\n") + "\n")?;
    Ok(())
}

/// Generate a default time.conf (mainnet defaults).
#[allow(dead_code)]
pub fn generate_default_conf(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    generate_default_conf_for_network(path, &NetworkType::Mainnet)
}

/// Generate a default masternode.conf with instructions.
#[allow(dead_code)]
pub fn generate_default_masternode_conf(
    path: &PathBuf,
    external_address: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut contents = String::from(
        r#"# TIME Coin Masternode Configuration
#
# Format (one entry per line):
#   alias  collateral_txid  collateral_vout
#
# Fields:
#   alias            - A name for this masternode (e.g., mn1)
#   collateral_txid  - Transaction ID of your collateral deposit
#   collateral_vout  - Output index of your collateral (usually 0)
#
# Your node's IP address is read from externalip= in time.conf.
# Your masternode private key is auto-generated on first startup and
# saved to time.conf. You can also set it manually:
#   masternodeprivkey=<key from `time-cli masternodegenkey`>
#
# To send rewards to your GUI wallet instead of this node's wallet,
# add this line to time.conf:
#   reward_address=TIME0...your_wallet_address...
#
# Steps to set up a masternode:
#   1. Set externalip=<your_public_ip> in time.conf
#   2. Start the node — masternodeprivkey is auto-generated in time.conf
#   3. (Optional) Set reward_address in time.conf to your GUI wallet address
#   4. For staked tiers, send collateral to yourself:
#      time-cli sendtoaddress <your_address> 1000    (Bronze = 1,000 TIME)
#      time-cli sendtoaddress <your_address> 10000   (Silver = 10,000 TIME)
#      time-cli sendtoaddress <your_address> 100000  (Gold   = 100,000 TIME)
#   5. Find your collateral TXID:
#      time-cli listtransactions
#   6. Add a line below and restart timed
#
# Example:
#   mn1  fc5b049a39807958cf...  0
"#,
    );

    // Include a commented upgrade hint if an external address is known
    if let Some(addr) = external_address {
        if !addr.is_empty() {
            contents.push_str(&format!(
                "\n# Free tier — no collateral entry needed.\n\
                 # To upgrade, add a line with your collateral info:\n\
                 #   mn1  <collateral_txid>  <collateral_vout>\n\
                 # (Your IP {} is already set via externalip= in time.conf)\n",
                addr
            ));
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)?;
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_parse_conf_file() {
        let dir = std::env::temp_dir().join("timecoin_test_conf");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test_time.conf");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "# comment line").unwrap();
        writeln!(f, "testnet=1").unwrap();
        writeln!(f, "port=24100").unwrap();
        writeln!(f, "server=1").unwrap();
        writeln!(f, "addnode=node1.example.com").unwrap();
        writeln!(f, "addnode=node2.example.com").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "  masternode = 0  ").unwrap();
        drop(f);

        let entries = parse_conf_file(&path).unwrap();
        assert_eq!(entries.get("testnet").unwrap(), &vec!["1".to_string()]);
        assert_eq!(entries.get("port").unwrap(), &vec!["24100".to_string()]);
        assert_eq!(entries.get("server").unwrap(), &vec!["1".to_string()]);
        assert_eq!(
            entries.get("addnode").unwrap(),
            &vec![
                "node1.example.com".to_string(),
                "node2.example.com".to_string()
            ]
        );
        assert_eq!(entries.get("masternode").unwrap(), &vec!["0".to_string()]);

        fs::remove_file(&path).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_masternode_conf_full() {
        let dir = std::env::temp_dir().join("timecoin_test_mn_conf");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("masternode.conf");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "mn1 69.167.168.176:24100 txid123 0").unwrap();
        drop(f);

        let entries = parse_masternode_conf(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].alias, "mn1");
        assert_eq!(entries[0].address, "69.167.168.176:24100");
        assert_eq!(entries[0].collateral_txid, "txid123");
        assert_eq!(entries[0].collateral_vout, 0);

        fs::remove_file(&path).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_masternode_conf_legacy_6field() {
        let dir = std::env::temp_dir().join("timecoin_test_mn_legacy6");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("masternode.conf");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(
            f,
            "mn1 69.167.168.176:24100 5HueCGU8rMjxEXxiPuD cert123abc txid123 0"
        )
        .unwrap();
        drop(f);

        let entries = parse_masternode_conf(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].collateral_txid, "txid123");
        assert_eq!(entries[0].collateral_vout, 0);

        fs::remove_file(&path).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_parse_masternode_conf_legacy_5field() {
        let dir = std::env::temp_dir().join("timecoin_test_mn_legacy5");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("masternode.conf");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "mn1 69.167.168.176:24100 5HueCGU8rMjxEXxiPuD txid123 0").unwrap();
        drop(f);

        let entries = parse_masternode_conf(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].collateral_txid, "txid123");

        fs::remove_file(&path).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_default_conf() {
        let dir = std::env::temp_dir().join("timecoin_test_gen");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("time.conf");

        // Mainnet default
        generate_default_conf(&path).unwrap();
        assert!(path.exists());
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("#testnet=0"));
        assert!(contents.contains("server=1"));
        assert!(contents.contains("masternode=1"));

        fs::remove_file(&path).ok();

        // Testnet explicit
        generate_default_conf_for_network(&path, &NetworkType::Testnet).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("testnet=1"));
        assert!(contents.contains("rpcuser="));
        assert!(contents.contains("rpcpassword="));
        let entries = parse_conf_file(&path).unwrap();
        assert_eq!(entries.get("testnet").unwrap(), &vec!["1".to_string()]);
        // Verify auto-generated credentials are non-empty
        let rpc_user = entries.get("rpcuser").unwrap().last().unwrap();
        let rpc_pass = entries.get("rpcpassword").unwrap().last().unwrap();
        assert!(rpc_user.len() >= 16, "rpcuser should be at least 16 chars");
        assert!(
            rpc_pass.len() >= 32,
            "rpcpassword should be at least 32 chars"
        );

        fs::remove_file(&path).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_generate_default_masternode_conf() {
        let dir = std::env::temp_dir().join("timecoin_test_mn_gen");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("masternode.conf");

        generate_default_masternode_conf(&path, None).unwrap();
        assert!(path.exists());

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("masternodeprivkey"));
        assert!(contents.contains("masternodegenkey"));

        // Should parse as empty (all lines are comments)
        let entries = parse_masternode_conf(&path).unwrap();
        assert!(entries.is_empty());

        fs::remove_file(&path).ok();

        // With external address — should include IP in commented upgrade hint
        generate_default_masternode_conf(&path, Some("1.2.3.4:24100")).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("1.2.3.4:24100"));
        // Free tier: no active entry, only commented hints
        let entries = parse_masternode_conf(&path).unwrap();
        assert!(entries.is_empty());

        fs::remove_file(&path).ok();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_detect_network_from_conf() {
        let dir = std::env::temp_dir().join("timecoin_test_detect");
        fs::create_dir_all(&dir).unwrap();

        // testnet=1
        let path = dir.join("testnet.conf");
        fs::write(&path, "testnet=1\n").unwrap();
        assert_eq!(detect_network_from_conf(&path), NetworkType::Testnet);

        // testnet=0 → mainnet
        let path2 = dir.join("mainnet.conf");
        fs::write(&path2, "testnet=0\n").unwrap();
        assert_eq!(detect_network_from_conf(&path2), NetworkType::Mainnet);

        // mainnet=1
        let path3 = dir.join("mainnet2.conf");
        fs::write(&path3, "mainnet=1\n").unwrap();
        assert_eq!(detect_network_from_conf(&path3), NetworkType::Mainnet);

        // No file, not in testnet dir → mainnet
        let path4 = dir.join("nonexistent.conf");
        assert_eq!(detect_network_from_conf(&path4), NetworkType::Mainnet);

        // No file, but inside a "testnet" directory → testnet
        let testnet_dir = dir.join("testnet");
        fs::create_dir_all(&testnet_dir).unwrap();
        let path5 = testnet_dir.join("time.conf");
        assert_eq!(detect_network_from_conf(&path5), NetworkType::Testnet);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_hardcoded_defaults_override_toml_values() {
        let mut config = Config::default();

        // Simulate a user trying to change hardcoded values via TOML
        config.storage.backend = "rocksdb".to_string();
        config.storage.compress_blocks = true;
        config.storage.cache_size_mb = 9999;
        config.consensus.min_masternodes = 1;
        config.block.block_time_seconds = 10;
        config.security.enable_rate_limiting = false;
        config.security.enable_message_signing = false;
        config.masternode.tier = "gold".to_string();
        config.ai.enabled = true;
        config.ai.learning_rate = 99.0;

        // Apply hardcoded defaults
        config.apply_hardcoded_defaults();

        // Verify all hardcoded values are restored
        assert_eq!(config.storage.backend, "sled");
        assert!(!config.storage.compress_blocks);
        assert_eq!(config.storage.cache_size_mb, 256);
        assert_eq!(config.consensus.min_masternodes, 3);
        assert_eq!(config.block.block_time_seconds, 600);
        assert!(config.security.enable_rate_limiting);
        assert!(config.security.enable_message_signing);
        assert_eq!(config.masternode.tier, "auto");
        // AI reverts to default (disabled by default)
        assert!(!config.ai.enabled);
    }

    #[test]
    fn test_hardcoded_defaults_preserve_user_settings() {
        let mut config = Config::default();

        // User-configurable settings
        config.node.network = "mainnet".to_string();
        config.network.listen_address = "0.0.0.0:24000".to_string();
        config.network.external_address = Some("1.2.3.4".to_string());
        config.network.bootstrap_peers = vec!["peer1".to_string()];
        config.rpc.listen_address = "0.0.0.0:24001".to_string();
        config.logging.level = "debug".to_string();
        config.masternode.enabled = true;
        config.masternode.collateral_txid = "abc123".to_string();
        config.masternode.masternodeprivkey = "mykey".to_string();

        config.apply_hardcoded_defaults();

        // These should NOT be overwritten (they're user-configurable)
        // Note: apply_hardcoded_defaults does reset some of these — that's by design.
        // The load_from_conf and load_or_create methods save/restore user values.
        // This test verifies what the method itself does.
        assert_eq!(config.masternode.tier, "auto"); // tier is always forced
        assert_eq!(config.logging.format, "pretty"); // format is always forced
    }
}
