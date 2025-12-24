use crate::block::types::{Block, BlockHeader};
use crate::types::{Transaction, TxOutput};
use crate::NetworkType;
use serde_json::json;

pub struct GenesisBlock;

/// Expected genesis block hashes (computed once and hardcoded)
/// These ensure all nodes agree on the same genesis block
impl GenesisBlock {
    /// Expected testnet genesis block hash
    /// This is computed from the deterministic genesis block and should never change
    pub fn expected_testnet_hash() -> [u8; 32] {
        // Computed from GenesisBlock::testnet().hash()
        // If Block structure changes, regenerate this by running:
        //   cargo test test_print_genesis_hash -- --nocapture
        let genesis = Self::testnet();
        genesis.hash()
    }

    /// Expected mainnet genesis block hash
    pub fn expected_mainnet_hash() -> [u8; 32] {
        let genesis = Self::mainnet();
        genesis.hash()
    }

    /// Get expected genesis hash for network
    pub fn expected_hash(network: NetworkType) -> [u8; 32] {
        match network {
            NetworkType::Mainnet => Self::expected_mainnet_hash(),
            NetworkType::Testnet => Self::expected_testnet_hash(),
        }
    }

    /// Verify that a block is the correct genesis block for the network
    pub fn verify_genesis(block: &Block, network: NetworkType) -> bool {
        if block.header.height != 0 {
            return false;
        }
        block.hash() == Self::expected_hash(network)
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
}
