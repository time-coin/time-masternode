use crate::block::types::{Block, BlockHeader};
use crate::types::{Transaction, TxOutput};
use crate::vdf::VDFProof;
use crate::NetworkType;
use serde_json::json;

pub struct GenesisBlock;

impl GenesisBlock {
    /// Testnet genesis block - December 1, 2025
    pub fn testnet() -> Block {
        let genesis_timestamp = 1733011200; // 2025-12-01T00:00:00Z

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
            },
            transactions: vec![coinbase],
            masternode_rewards: vec![],
            vdf_proof: VDFProof {
                output: vec![0u8; 32],
                iterations: 100_000,
                checkpoints: vec![],
            },
        }
    }

    /// Mainnet genesis block - TBD
    pub fn mainnet() -> Block {
        let genesis_timestamp = 1735689600; // 2025-01-01T00:00:00Z (placeholder)

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
            },
            transactions: vec![coinbase],
            masternode_rewards: vec![],
            vdf_proof: VDFProof {
                output: vec![0u8; 32],
                iterations: 100_000,
                checkpoints: vec![],
            },
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
                "TIME Coin {} Genesis Block - Proof of Time Enabled",
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
                    "proof_of_time": {
                        "output": hex::encode(&block.vdf_proof.output),
                        "iterations": block.vdf_proof.iterations,
                        "checkpoints": &block.vdf_proof.checkpoints,
                    }
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
}
