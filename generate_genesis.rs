// Temporary program to generate canonical genesis block for testnet
// Run with: cargo run --bin generate_genesis

use serde_json;
use std::fs;

// We'll manually construct the genesis block JSON since we can't easily
// run a binary that depends on the full crate from here

fn main() {
    // Define canonical masternodes (sorted by IP)
    let masternodes = vec![
        ("50.28.104.50", "Free"),
        ("64.91.241.10", "Free"),
        ("69.167.168.176", "Free"),
        ("165.84.215.117", "Free"),
    ];

    let leader = "50.28.104.50";
    let timestamp = 1764547200i64; // 2025-12-01T00:00:00Z
    let block_reward = 10_000_000_000u64; // 100 TIME

    // Each Free tier masternode gets equal share
    let reward_per_mn = block_reward / masternodes.len() as u64;
    let last_mn_reward = block_reward - (reward_per_mn * (masternodes.len() - 1) as u64);

    // Calculate merkle root (txid of empty coinbase)
    let coinbase_txid = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"; // SHA256 of empty

    // Build masternode rewards
    let mut rewards = Vec::new();
    for (i, (addr, _tier)) in masternodes.iter().enumerate() {
        let reward = if i == masternodes.len() - 1 {
            last_mn_reward
        } else {
            reward_per_mn
        };
        rewards.push((addr.to_string(), reward));
    }

    let genesis = serde_json::json!({
        "header": {
            "version": 2,
            "height": 0,
            "previous_hash": [0u8; 32],
            "merkle_root": hex::decode(coinbase_txid).unwrap(),
            "timestamp": timestamp,
            "block_reward": block_reward,
            "leader": leader,
            "attestation_root": [0u8; 32],
            "masternode_tiers": {
                "free": 4,
                "bronze": 0,
                "silver": 0,
                "gold": 0
            }
        },
        "transactions": [{
            "version": 1,
            "inputs": [],
            "outputs": [],
            "lock_time": 0,
            "timestamp": timestamp
        }],
        "masternode_rewards": rewards,
        "time_attestations": []
    });

    let json_str = serde_json::to_string_pretty(&genesis).unwrap();
    fs::write("genesis.testnet.json", json_str).expect("Failed to write genesis.testnet.json");
    
    println!("✅ Canonical genesis block saved to genesis.testnet.json");
}
