//! Configuration management for TIME Coin daemon.
//!
//! Note: Some items appear as "dead code" in library checks because they're
//! only used by the binary (main.rs). These include:
//! - `get_data_dir()`, `get_network_data_dir()` - used for config path resolution
//! - `NodeConfig::network_type()` - used to determine network type from config
//! - `Config::load_from_file()`, etc. - used for config persistence

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::network_type::NetworkType;

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
    /// Allow this node to produce catchup blocks when behind consensus
    #[serde(default = "default_false")]
    pub enable_catchup_blocks: bool,
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
    pub tier: String,
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
    pub transaction_analyzer: AITransactionAnalyzerConfig,

    #[serde(default)]
    pub network_optimizer: AINetworkOptimizerConfig,

    #[serde(default)]
    pub predictive_sync: AIPredictiveSyncConfig,

    #[serde(default)]
    pub resource_manager: AIResourceManagerConfig,
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
            transaction_analyzer: AITransactionAnalyzerConfig::default(),
            network_optimizer: AINetworkOptimizerConfig::default(),
            predictive_sync: AIPredictiveSyncConfig::default(),
            resource_manager: AIResourceManagerConfig::default(),
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

/// AI Transaction Analyzer Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AITransactionAnalyzerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AITransactionAnalyzerConfig {
    fn default() -> Self {
        Self { enabled: true }
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

/// AI Resource Manager Configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AIResourceManagerConfig {
    #[serde(default = "default_false")]
    pub enabled: bool,
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
                enable_catchup_blocks: false,
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
                listen_address: "127.0.0.1".to_string(),
                allow_origins: vec!["http://localhost:3000".to_string()],
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
                enabled: false,
                collateral_txid: String::new(),
                tier: "silver".to_string(),
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
}
