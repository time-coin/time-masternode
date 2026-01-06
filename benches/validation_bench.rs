//! Benchmarks for block validation performance
//!
//! Run with: cargo bench

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use timed::block::types::{Block, BlockHeader};
use timed::blockchain_validation::BlockValidator;
use timed::network_type::NetworkType;

fn create_test_block(height: u64) -> Block {
    let transactions = vec![];
    let merkle_root = timed::block::types::calculate_merkle_root(&transactions);

    Block {
        header: BlockHeader {
            version: 1,
            height,
            previous_hash: [0u8; 32],
            merkle_root,
            timestamp: chrono::Utc::now().timestamp(),
            block_reward: 0,
            leader: "test".to_string(),
            attestation_root: [0u8; 32],
            masternode_tiers: Default::default(),
        },
        transactions,
        masternode_rewards: vec![],
        time_attestations: vec![],
    }
}

fn bench_block_validation(c: &mut Criterion) {
    let validator = BlockValidator::new(NetworkType::Testnet);
    let block = create_test_block(1);

    c.bench_function("validate_empty_block", |b| {
        b.iter(|| {
            validator
                .validate_block(black_box(&block), Some([0u8; 32]))
                .unwrap()
        })
    });
}

fn bench_block_validation_with_txs(c: &mut Criterion) {
    let validator = BlockValidator::new(NetworkType::Testnet);

    // Create block with transactions
    let mut block = create_test_block(1);

    // Add some dummy transactions
    for i in 0..100 {
        let tx = timed::types::Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![timed::types::TxOutput {
                value: 100,
                script_pubkey: format!("addr_{}", i).into_bytes(),
            }],
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
        };
        block.transactions.push(tx);
    }

    // Recalculate merkle root
    block.header.merkle_root = timed::block::types::calculate_merkle_root(&block.transactions);

    c.bench_function("validate_block_with_100_txs", |b| {
        b.iter(|| {
            validator
                .validate_block(black_box(&block), Some([0u8; 32]))
                .unwrap()
        })
    });
}

fn bench_chain_sequence_validation(c: &mut Criterion) {
    let validator = BlockValidator::new(NetworkType::Testnet);

    // Create a chain of 10 blocks
    let mut blocks = vec![];
    let mut prev_hash = [0u8; 32];

    for height in 1..=10 {
        let mut block = create_test_block(height);
        block.header.previous_hash = prev_hash;
        prev_hash = block.hash();
        blocks.push(block);
    }

    c.bench_function("validate_chain_sequence_10_blocks", |b| {
        b.iter(|| {
            validator
                .validate_chain_sequence(black_box(&blocks))
                .unwrap()
        })
    });
}

criterion_group!(
    benches,
    bench_block_validation,
    bench_block_validation_with_txs,
    bench_chain_sequence_validation
);
criterion_main!(benches);
