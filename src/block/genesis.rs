use crate::block::types::{Block, BlockHeader};
use crate::types::{Transaction, TxOutput};
use crate::NetworkType;
use serde_json::json;

pub struct GenesisBlock;

/// Expected genesis block hashes - HARDCODED for network consensus
/// These are the canonical genesis hashes that all nodes must agree on
impl GenesisBlock {
    /// Testnet genesis block hash (from genesis.testnet.json)
    /// This MUST match the hash in genesis.testnet.json
    pub const TESTNET_GENESIS_HASH: &'static str =
        "59f1b60c1bbf195d30b19c0ead4aab1c663c49ed56ff8ee7030e6a4a7a7415af";

    /// Mainnet genesis block hash (from genesis.mainnet.json when created)
    pub const MAINNET_GENESIS_HASH: &'static str =
        "c2853890e1e84312724a4f2fc132b6c77a742550b5cabd1745e3e6437bd3fc2a";

    /// Get expected genesis hash for network as bytes
    pub fn expected_hash(network: NetworkType) -> [u8; 32] {
        let hex_str = match network {
            NetworkType::Testnet => Self::TESTNET_GENESIS_HASH,
            NetworkType::Mainnet => Self::MAINNET_GENESIS_HASH,
        };
        let mut hash = [0u8; 32];
        if let Ok(bytes) = hex::decode(hex_str) {
            if bytes.len() == 32 {
                hash.copy_from_slice(&bytes);
            }
        }
        hash
    }

    /// Verify that a block is the correct genesis block for the network
    pub fn verify_genesis(block: &Block, network: NetworkType) -> bool {
        if block.header.height != 0 {
            return false;
        }
        block.hash() == Self::expected_hash(network)
    }

    /// Load genesis block from JSON file
    pub fn load_from_file(path: &str) -> Result<Block, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read genesis file {}: {}", path, e))?;

        let json: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse genesis JSON: {}", e))?;

        let block_json = json
            .get("block")
            .ok_or("Missing 'block' field in genesis JSON")?;

        Self::parse_block_json(block_json)
    }

    /// Parse a block from JSON value
    fn parse_block_json(json: &serde_json::Value) -> Result<Block, String> {
        let header = json.get("header").ok_or("Missing 'header' in block JSON")?;

        let transactions = json
            .get("transactions")
            .and_then(|t| t.as_array())
            .ok_or("Missing 'transactions' in block JSON")?;

        let mut txs = Vec::new();
        for tx_json in transactions {
            txs.push(Self::parse_transaction_json(tx_json)?);
        }

        let previous_hash = Self::parse_hash(
            header
                .get("previous_hash")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )?;

        let attestation_root = Self::parse_hash(
            header
                .get("attestation_root")
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )?;

        let merkle_root = if txs.is_empty() {
            [0u8; 32]
        } else {
            txs[0].txid()
        };

        Ok(Block {
            header: BlockHeader {
                version: header.get("version").and_then(|v| v.as_u64()).unwrap_or(2) as u32,
                height: header.get("height").and_then(|v| v.as_u64()).unwrap_or(0),
                previous_hash,
                merkle_root,
                timestamp: header
                    .get("timestamp_unix")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                block_reward: header
                    .get("block_reward")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                leader: header
                    .get("leader")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                attestation_root,
            },
            transactions: txs,
            masternode_rewards: vec![],
            time_attestations: vec![],
        })
    }

    fn parse_transaction_json(json: &serde_json::Value) -> Result<Transaction, String> {
        let outputs = json
            .get("outputs")
            .and_then(|o| o.as_array())
            .ok_or("Missing 'outputs' in transaction")?;

        let mut tx_outputs = Vec::new();
        for out in outputs {
            let script_hex = out
                .get("script_pubkey")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let script_pubkey =
                hex::decode(script_hex).map_err(|e| format!("Invalid script_pubkey hex: {}", e))?;

            tx_outputs.push(TxOutput {
                value: out.get("value").and_then(|v| v.as_u64()).unwrap_or(0),
                script_pubkey,
            });
        }

        Ok(Transaction {
            version: json.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32,
            inputs: vec![],
            outputs: tx_outputs,
            lock_time: json.get("lock_time").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            timestamp: json.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0),
        })
    }

    fn parse_hash(hex_str: &str) -> Result<[u8; 32], String> {
        let mut hash = [0u8; 32];
        if hex_str.is_empty()
            || hex_str == "0000000000000000000000000000000000000000000000000000000000000000"
        {
            return Ok(hash);
        }
        let bytes =
            hex::decode(hex_str).map_err(|e| format!("Invalid hash hex '{}': {}", hex_str, e))?;
        if bytes.len() != 32 {
            return Err(format!("Hash must be 32 bytes, got {}", bytes.len()));
        }
        hash.copy_from_slice(&bytes);
        Ok(hash)
    }
}

impl GenesisBlock {
    /// Testnet genesis block - December 1, 2025
    pub fn testnet() -> Block {
        let genesis_timestamp = 1764547200; // 2025-12-01T00:00:00Z

        // Coinbase transaction with genesis reward
        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 10_000_000_000, // 100 TIME in satoshis (100 * 10^8)
                script_pubkey: b"genesis".to_vec(),
            }],
            lock_time: 0,
            timestamp: genesis_timestamp,
        };

        Block {
            header: BlockHeader {
                version: 2,
                height: 0,
                previous_hash: [0u8; 32],
                merkle_root: coinbase.txid(),
                timestamp: genesis_timestamp,
                block_reward: 10_000_000_000, // 100 TIME in satoshis
                leader: String::new(),
                attestation_root: [0u8; 32],
            },
            transactions: vec![coinbase],
            masternode_rewards: vec![],
            time_attestations: vec![],
        }
    }

    /// Mainnet genesis block - January 1, 2026
    pub fn mainnet() -> Block {
        let genesis_timestamp = 1767225600; // 2026-01-01T00:00:00Z

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 0, // No pre-mine on mainnet
                script_pubkey: b"genesis".to_vec(),
            }],
            lock_time: 0,
            timestamp: genesis_timestamp,
        };

        Block {
            header: BlockHeader {
                version: 2,
                height: 0,
                previous_hash: [0u8; 32],
                merkle_root: coinbase.txid(),
                timestamp: genesis_timestamp,
                block_reward: 0,
                leader: String::new(),
                attestation_root: [0u8; 32],
            },
            transactions: vec![coinbase],
            masternode_rewards: vec![],
            time_attestations: vec![],
        }
    }

    /// Get genesis block for the specified network
    pub fn for_network(network: NetworkType) -> Block {
        match network {
            NetworkType::Mainnet => Self::mainnet(),
            NetworkType::Testnet => Self::testnet(),
        }
    }

    /// Export genesis block as JSON (for documentation)
    #[allow(dead_code)]
    pub fn export_json(network: NetworkType) -> String {
        let block = Self::for_network(network);
        let block_hash = block.hash();
        let network_str = match network {
            NetworkType::Mainnet => "mainnet",
            NetworkType::Testnet => "testnet",
        };

        json!({
            "network": network_str,
            "version": 2,
            "message": format!(
                "TIME Coin {} Genesis Block - TSDC + Avalanche Consensus",
                if matches!(network, NetworkType::Mainnet) { "Mainnet" } else { "Testnet" }
            ),
            "block": {
                "header": {
                    "block_number": block.header.height,
                    "timestamp": chrono::DateTime::from_timestamp(block.header.timestamp, 0)
                        .unwrap()
                        .format("%Y-%m-%dT%H:%M:%SZ")
                        .to_string(),
                    "previous_hash": hex::encode(block.header.previous_hash),
                    "merkle_root": hex::encode(block.header.merkle_root),
                    "block_reward": block.header.block_reward,
                },
                "transactions": block.transactions.iter().map(|tx| {
                    json!({
                        "txid": hex::encode(tx.txid()),
                        "version": tx.version,
                        "inputs": tx.inputs,
                        "outputs": tx.outputs.iter().map(|o| {
                            json!({
                                "amount": o.value,
                                "script_pubkey": hex::encode(&o.script_pubkey),
                            })
                        }).collect::<Vec<_>>(),
                        "lock_time": tx.lock_time,
                        "timestamp": tx.timestamp,
                    })
                }).collect::<Vec<_>>(),
                "hash": hex::encode(block_hash),
            }
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block_testnet() {
        let genesis = GenesisBlock::testnet();
        assert_eq!(genesis.header.height, 0);
        assert_eq!(genesis.header.previous_hash, [0u8; 32]);
        assert_eq!(genesis.transactions.len(), 1);
        assert_eq!(genesis.transactions[0].outputs[0].value, 10_000_000_000); // 100 TIME in satoshis
    }

    #[test]
    fn test_genesis_block_deterministic() {
        let genesis1 = GenesisBlock::testnet();
        let genesis2 = GenesisBlock::testnet();
        assert_eq!(genesis1.hash(), genesis2.hash());
    }

    #[test]
    fn test_genesis_verification() {
        let genesis = GenesisBlock::testnet();
        assert!(GenesisBlock::verify_genesis(&genesis, NetworkType::Testnet));

        let mainnet_genesis = GenesisBlock::mainnet();
        assert!(GenesisBlock::verify_genesis(
            &mainnet_genesis,
            NetworkType::Mainnet
        ));

        // Cross-network verification should fail
        assert!(!GenesisBlock::verify_genesis(
            &genesis,
            NetworkType::Mainnet
        ));
        assert!(!GenesisBlock::verify_genesis(
            &mainnet_genesis,
            NetworkType::Testnet
        ));
    }

    #[test]
    fn test_print_genesis_hash() {
        // Run with: cargo test test_print_genesis_hash -- --nocapture
        let testnet = GenesisBlock::testnet();
        let mainnet = GenesisBlock::mainnet();

        println!("=== Genesis Block Hashes ===");
        println!("Testnet genesis hash: {}", hex::encode(testnet.hash()));
        println!("Mainnet genesis hash: {}", hex::encode(mainnet.hash()));
        println!("===========================");
    }

    #[test]
    fn test_hardcoded_hash_matches_computed() {
        // Verify hardcoded hash constants match computed genesis hashes
        let testnet = GenesisBlock::testnet();
        let mainnet = GenesisBlock::mainnet();

        assert_eq!(
            hex::encode(testnet.hash()),
            GenesisBlock::TESTNET_GENESIS_HASH,
            "Testnet genesis hash mismatch! Update TESTNET_GENESIS_HASH constant."
        );

        assert_eq!(
            hex::encode(mainnet.hash()),
            GenesisBlock::MAINNET_GENESIS_HASH,
            "Mainnet genesis hash mismatch! Update MAINNET_GENESIS_HASH constant."
        );
    }
}
