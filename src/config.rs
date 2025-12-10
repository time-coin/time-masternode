use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::network_type::NetworkType;

/// Get the platform-specific data directory for TIME Coin
pub fn get_data_dir() -> PathBuf {
    if cfg!(windows) {
        // Windows: %APPDATA%\timecoin
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("timecoin")
    } else {
        // Linux/Mac: ~/.timecoin
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".timecoin")
    }
}

/// Get the network-specific subdirectory (mainnet or testnet)
pub fn get_network_data_dir(network: &NetworkType) -> PathBuf {
    let base = get_data_dir();
    match network {
        NetworkType::Mainnet => base.join("mainnet"),
        NetworkType::Testnet => base.join("testnet"),
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
    pub metrics: MetricsConfig,
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
    pub max_peers: u32,
    pub enable_upnp: bool,
    pub enable_peer_discovery: bool,
    pub bootstrap_peers: Vec<String>,
}

impl NetworkConfig {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    pub enabled: bool,
    pub listen_address: String,
    pub allow_origins: Vec<String>,
}

impl RpcConfig {
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
    pub voting_timeout_ms: u64,
    pub min_masternodes: u32,
    pub quorum_percentage: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockConfig {
    pub target_time_utc: String,
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
    pub wallet_address: String,
    pub collateral_txid: String,
    pub tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    pub enable_rate_limiting: bool,
    pub max_requests_per_second: u32,
    pub enable_authentication: bool,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub prometheus_port: u16,
}

impl Config {
    pub fn load_from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    pub fn default() -> Self {
        Self {
            node: NodeConfig {
                name: "TIME Coin Node".to_string(),
                version: "0.1.0".to_string(),
                network: "testnet".to_string(),
            },
            network: NetworkConfig {
                listen_address: "0.0.0.0".to_string(),
                max_peers: 50,
                enable_upnp: false,
                enable_peer_discovery: true,
                bootstrap_peers: vec![],
            },
            rpc: RpcConfig {
                enabled: true,
                listen_address: "127.0.0.1".to_string(),
                allow_origins: vec!["http://localhost:3000".to_string()],
            },
            storage: StorageConfig {
                backend: "memory".to_string(),
                data_dir: "./data".to_string(),
                cache_size_mb: 256,
            },
            consensus: ConsensusConfig {
                voting_timeout_ms: 3000,
                min_masternodes: 3,
                quorum_percentage: 67,
            },
            block: BlockConfig {
                target_time_utc: "00:00:00".to_string(),
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
                wallet_address: String::new(),
                collateral_txid: String::new(),
                tier: "silver".to_string(),
            },
            security: SecurityConfig {
                enable_rate_limiting: true,
                max_requests_per_second: 1000,
                enable_authentication: false,
                api_key: String::new(),
            },
            metrics: MetricsConfig {
                enabled: false,
                prometheus_port: 9090,
            },
        }
    }

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

            // Update data_dir to use platform-specific path
            config.storage.data_dir = data_dir.to_string_lossy().to_string();

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

    pub fn save_to_file(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let contents = toml::to_string_pretty(self)?;
        fs::write(path, contents)?;
        Ok(())
    }
}
