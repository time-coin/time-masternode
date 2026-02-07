//! TimeProof Conflict Detection Tests
//!
//! Comprehensive tests for Pre-Mainnet Checklist Item 9:
//! "Conflicting TimeProof detection"
//!
//! **IMPORTANT:** This is NOT for preventing double-spends.
//! UTXO locking already prevents two transactions from both spending the same UTXO.
//!
//! This detection is for:
//! - Detecting implementation bugs in UTXO state machine
//! - Monitoring Byzantine validator behavior (equivocation)
//! - Detecting stale proofs from network partitions
//! - Security monitoring and alerting
//!
//! Tests cover:
//! - Detection when multiple TimeProofs exist for same txid (should be rare/impossible)
//! - Stale proof detection after partition healing
//! - Conflict logging and metrics
//! - Integration with anomaly detector

use std::sync::Arc;
use timed::consensus::{TimeVoteConfig, TimeVoteConsensus};
use timed::masternode_registry::MasternodeRegistry;
use timed::network_type::NetworkType;
use timed::types::{Hash256, TimeProof, TimeVote, VoteDecision};

// ============================================================================
// Test Utilities
// ============================================================================

fn create_test_masternode_registry() -> Arc<MasternodeRegistry> {
    let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
    Arc::new(MasternodeRegistry::new(db, NetworkType::Testnet))
}

fn create_test_consensus() -> Arc<TimeVoteConsensus> {
    let config = TimeVoteConfig::default();
    let registry = create_test_masternode_registry();
    Arc::new(TimeVoteConsensus::new(config, registry).expect("Failed to create consensus engine"))
}

fn create_dummy_hash(id: u64) -> Hash256 {
    let mut hash = [0u8; 32];
    hash[0..8].copy_from_slice(&id.to_le_bytes());
    hash
}

fn create_test_vote(
    voter_id: &str,
    weight: u64,
    txid: Hash256,
    decision: VoteDecision,
) -> TimeVote {
    TimeVote {
        chain_id: 1,
        txid,
        tx_hash_commitment: create_dummy_hash(1),
        slot_index: 0,
        decision,
        voter_mn_id: voter_id.to_string(),
        voter_weight: weight,
        signature: vec![0u8; 64], // Dummy signature
    }
}

fn create_test_timeproof(txid: Hash256, votes: Vec<TimeVote>, slot_index: u64) -> TimeProof {
    TimeProof {
        txid,
        slot_index,
        votes,
    }
}

// ============================================================================
// Normal Operation Tests (No Conflicts Expected)
// ============================================================================

#[tokio::test]
async fn test_single_timeproof_no_conflict() {
    let consensus = create_test_consensus();
    let txid = create_dummy_hash(1);

    let vote = create_test_vote("validator1", 1000, txid, VoteDecision::Accept);
    let proof = create_test_timeproof(txid, vec![vote], 0);

    let result = consensus.detect_competing_timeproof(proof, 1000);
    assert!(result.is_ok(), "Should succeed with single proof");

    let winning_index = result.unwrap();
    assert_eq!(winning_index, 0, "Single proof should be at index 0");

    // Check no conflict was recorded
    let conflicts = consensus.get_competing_timeproofs(txid);
    assert_eq!(conflicts.len(), 1, "Should have exactly 1 proof");

    // Verify no conflict info logged (since only 1 proof)
    let conflict_info = consensus.get_conflict_info(txid, 0);
    assert!(conflict_info.is_none(), "No conflict info for single proof");
}

// ============================================================================
// Bug Detection Tests (Competing Proofs = Indicates Bug/Byzantine Behavior)
// ============================================================================

#[tokio::test]
async fn test_competing_timeproofs_detected_as_anomaly() {
    // **ANOMALY:** Multiple TimeProofs for same TX indicates:
    // 1. UTXO state management bug, OR
    // 2. Byzantine validator equivocation (caught here), OR
    // 3. Stale proof from partition healing

    let consensus = create_test_consensus();
    let txid = create_dummy_hash(100);

    // First proof: 67% from validators 1,2,3 (weight: 3000)
    let mut votes_a = vec![];
    votes_a.push(create_test_vote("v1", 1000, txid, VoteDecision::Accept));
    votes_a.push(create_test_vote("v2", 1000, txid, VoteDecision::Accept));
    votes_a.push(create_test_vote("v3", 1000, txid, VoteDecision::Accept));
    let proof_a = create_test_timeproof(txid, votes_a, 5);

    // This should be the ONLY finalized proof
    let result1 = consensus.detect_competing_timeproof(proof_a, 3000);
    assert!(result1.is_ok());
    assert_eq!(result1.unwrap(), 0, "First proof at index 0");

    // If a SECOND proof appears, it indicates a problem
    // (Stale from partition, or implementation bug)
    let mut votes_b = vec![];
    votes_b.push(create_test_vote("v4", 1000, txid, VoteDecision::Accept));
    votes_b.push(create_test_vote("v5", 1000, txid, VoteDecision::Accept));
    let proof_b = create_test_timeproof(txid, votes_b, 5);

    let result2 = consensus.detect_competing_timeproof(proof_b, 2000);
    assert!(result2.is_ok());

    // Conflict should be detected and logged
    let conflict_info = consensus.get_conflict_info(txid, 5);
    assert!(conflict_info.is_some(), "Anomaly should be logged");

    let conflict = conflict_info.unwrap();
    assert_eq!(conflict.proof_count, 2, "Should record 2 proofs (anomaly)");
    assert!(
        !conflict.resolved,
        "Should be marked as requiring investigation"
    );

    // Metrics updated
    let detected = consensus.conflicts_detected_count();
    assert!(detected > 0, "Anomaly should increment metrics");
}

#[tokio::test]
async fn test_stale_proof_detection_from_partition() {
    // Scenario: Network partition healed, now seeing a stale proof
    // from the minority partition that lost consensus

    let consensus = create_test_consensus();
    let txid = create_dummy_hash(101);

    // Canonical proof from majority partition (2000 weight)
    let mut canonical_votes = vec![];
    canonical_votes.push(create_test_vote(
        "majority_v1",
        1000,
        txid,
        VoteDecision::Accept,
    ));
    canonical_votes.push(create_test_vote(
        "majority_v2",
        1000,
        txid,
        VoteDecision::Accept,
    ));
    let canonical = create_test_timeproof(txid, canonical_votes, 10);

    consensus
        .detect_competing_timeproof(canonical.clone(), 2000)
        .ok();

    // Stale proof from minority partition (800 weight) now arrives
    let mut stale_votes = vec![];
    stale_votes.push(create_test_vote(
        "minority_v1",
        400,
        txid,
        VoteDecision::Accept,
    ));
    stale_votes.push(create_test_vote(
        "minority_v2",
        400,
        txid,
        VoteDecision::Accept,
    ));
    let stale = create_test_timeproof(txid, stale_votes, 10);

    let winning = consensus
        .detect_competing_timeproof(stale, 800)
        .expect("Detection");

    // Canonical proof should win (higher weight)
    assert_eq!(winning, 0, "Canonical proof should win (higher weight)");

    // Conflict logged for monitoring
    let conflict = consensus.get_conflict_info(txid, 10).unwrap();
    assert_eq!(conflict.proof_count, 2, "Both proofs tracked");
    assert_eq!(conflict.max_weight, 2000, "Canonical is heavier");
    assert_eq!(conflict.winning_proof_index, 0, "Canonical should win");
}

// ============================================================================
// Metrics and Monitoring Tests
// ============================================================================

#[tokio::test]
async fn test_conflict_metrics_for_monitoring() {
    let consensus = create_test_consensus();

    let initial_count = consensus.conflicts_detected_count();

    // Create anomalies (detected conflicts)
    for i in 0..3 {
        let txid = create_dummy_hash(200 + i);

        // Create 2 proofs for same TX (anomaly)
        for j in 0..2 {
            let weight = 1000 + (j as u64 * 100);
            let vote = create_test_vote(&format!("v{}", j), weight, txid, VoteDecision::Accept);
            let proof = create_test_timeproof(txid, vec![vote], 0);
            consensus.detect_competing_timeproof(proof, weight).ok();
        }
    }

    let final_count = consensus.conflicts_detected_count();
    assert_eq!(final_count - initial_count, 3, "Should record 3 anomalies");
}

#[tokio::test]
async fn test_conflict_info_for_security_alerts() {
    let consensus = create_test_consensus();
    let txid = create_dummy_hash(210);

    // Multiple competing proofs detected (security alert scenario)
    let weights = vec![600, 800, 700, 900];

    for (i, weight) in weights.iter().enumerate() {
        let vote = create_test_vote(&format!("v{}", i), *weight, txid, VoteDecision::Accept);
        let proof = create_test_timeproof(txid, vec![vote], 0);
        consensus.detect_competing_timeproof(proof, *weight).ok();
    }

    let conflict = consensus.get_conflict_info(txid, 0).unwrap();

    // Verify alert contains needed info
    assert_eq!(conflict.proof_count, 4, "Alert shows all competing proofs");
    assert_eq!(conflict.proof_weights, weights, "All weights recorded");
    assert_eq!(conflict.max_weight, 900, "Maximum weight tracked");
    assert_eq!(conflict.winning_proof_index, 3, "Winner identified");

    // Can use for security dashboard
    assert!(conflict.detected_at > 0, "Timestamp for alert");
}

// ============================================================================
// Cleanup and Resolution Tests
// ============================================================================

#[tokio::test]
async fn test_clear_competing_timeproofs_after_investigation() {
    let consensus = create_test_consensus();
    let txid = create_dummy_hash(220);

    // Anomaly detected
    let vote1 = create_test_vote("v1", 1000, txid, VoteDecision::Accept);
    let proof1 = create_test_timeproof(txid, vec![vote1], 0);
    consensus.detect_competing_timeproof(proof1, 1000).ok();

    let vote2 = create_test_vote("v2", 800, txid, VoteDecision::Accept);
    let proof2 = create_test_timeproof(txid, vec![vote2], 0);
    consensus.detect_competing_timeproof(proof2, 800).ok();

    // Before cleanup
    assert_eq!(consensus.get_competing_timeproofs(txid).len(), 2);

    // After investigation/resolution, clear
    consensus.clear_competing_timeproofs(txid);
    assert_eq!(consensus.get_competing_timeproofs(txid).len(), 0);
}

#[tokio::test]
async fn test_fork_resolution_selects_canonical() {
    // When conflict detected, resolve to canonical proof (highest weight)

    let consensus = create_test_consensus();
    let txid = create_dummy_hash(230);

    // Create competing proofs
    let weights = vec![500, 1500, 800];

    for (i, weight) in weights.iter().enumerate() {
        let vote = create_test_vote(&format!("v{}", i), *weight, txid, VoteDecision::Accept);
        let proof = create_test_timeproof(txid, vec![vote], 0);
        consensus.detect_competing_timeproof(proof, *weight).ok();
    }

    let resolved = consensus.resolve_timeproof_fork(txid).unwrap();
    assert!(resolved.is_some());

    let winning = resolved.unwrap();
    assert_eq!(winning.slot_index, 0);

    // Verify resolution marked
    let conflict = consensus.get_conflict_info(txid, 0).unwrap();
    assert!(conflict.resolved, "Should mark as resolved");
}

// ============================================================================
// Integration with UTXO Validation (Should Prevent Reaching This)
// ============================================================================

#[tokio::test]
async fn test_competing_proofs_should_never_happen_normally() {
    // This test documents that competing proofs indicate a bug
    // Normal operation: UTXO locking prevents conflicting transactions from both finalizing

    let consensus = create_test_consensus();
    let txid = create_dummy_hash(300);

    // In normal operation, if two proofs exist:
    // - One was rejected at UTXO validation layer (should never finalize)
    // - This is a BUG indicator

    let vote1 = create_test_vote("v1", 2000, txid, VoteDecision::Accept);
    let proof1 = create_test_timeproof(txid, vec![vote1], 0);
    consensus.detect_competing_timeproof(proof1, 2000).ok();

    // If this reaches here, alert!
    let conflicts = consensus.get_competing_timeproofs(txid);
    if conflicts.len() > 1 {
        tracing::error!(
            "⚠️  PROTOCOL VIOLATION: Multiple proofs for single TX! UTXO state corruption?"
        );
        assert!(false, "Multiple proofs indicates UTXO state bug");
    }
}
