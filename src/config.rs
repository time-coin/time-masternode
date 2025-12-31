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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub name: String,
    pub version: String,
    #[serde(default = "default_network")]
    pub network: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusConfig {
    pub min_masternodes: u32,
    /// Use genesis block from file (genesis.testnet.json or genesis.mainnet.json)
    #[serde(default)]
    pub use_genesis_file: bool,
    /// Path to genesis file (relative to working directory or absolute)
    #[serde(default = "default_genesis_file")]
    pub genesis_file: String,
}

fn default_genesis_file() -> String {
    "genesis.testnet.json".to_string()
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
                version: "0.1.0".to_string(),
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
            },
            consensus: ConsensusConfig {
                min_masternodes: 3,
                use_genesis_file: true,
                genesis_file: "genesis.testnet.json".to_string(),
            },
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
