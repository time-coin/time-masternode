//! VRF (Verifiable Random Function) integration for blocks
//!
//! This module integrates ECVRF into block production and validation,
//! providing cryptographically secure chain comparison for fork resolution.

use crate::crypto::ecvrf::{ECVRFOutput, ECVRFProof, ECVRF};
use crate::types::Hash256;
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// Generate VRF proof and output for block production
///
/// # Arguments
/// * `signing_key` - Block leader's ed25519 signing key
/// * `height` - Block height (for determinism)
/// * `previous_hash` - Previous block hash (for chain context)
///
/// # Returns
/// Tuple of (vrf_proof, vrf_output, vrf_score)
pub fn generate_block_vrf(
    signing_key: &SigningKey,
    height: u64,
    previous_hash: &Hash256,
) -> (Vec<u8>, Hash256, u64) {
    // Create VRF input from block context
    let vrf_input = create_vrf_input(height, previous_hash);

    // Generate VRF proof and output
    match ECVRF::evaluate(signing_key, &vrf_input) {
        Ok((output, proof)) => {
            let vrf_output = output.bytes;
            let vrf_score = output.as_u64();
            let vrf_proof = proof.bytes.to_vec();

            (vrf_proof, vrf_output, vrf_score)
        }
        Err(e) => {
            tracing::error!("VRF generation failed: {}. Using fallback.", e);
            // Fallback: use deterministic hash
            fallback_vrf(height, previous_hash)
        }
    }
}

/// Verify VRF proof for a block
///
/// # Arguments
/// * `verifying_key` - Block leader's public key
/// * `height` - Block height
/// * `previous_hash` - Previous block hash
/// * `vrf_proof` - VRF proof from block header
/// * `vrf_output` - VRF output from block header
///
/// # Returns
/// Ok(()) if proof is valid, Err with description otherwise
pub fn verify_block_vrf(
    verifying_key: &VerifyingKey,
    height: u64,
    previous_hash: &Hash256,
    vrf_proof: &[u8],
    vrf_output: &Hash256,
) -> Result<(), String> {
    // Empty proof means old block (before VRF) - skip verification
    if vrf_proof.is_empty() {
        return Ok(());
    }

    // Verify proof length
    if vrf_proof.len() != 80 {
        return Err(format!(
            "Invalid VRF proof length: {} (expected 80)",
            vrf_proof.len()
        ));
    }

    // Convert proof to ECVRF format
    let mut proof_bytes = [0u8; 80];
    proof_bytes.copy_from_slice(vrf_proof);
    let proof = ECVRFProof::new(proof_bytes);

    // Create VRF input from block context
    let vrf_input = create_vrf_input(height, previous_hash);

    // Create output for verification
    let output = ECVRFOutput::new(*vrf_output);

    // Verify proof
    ECVRF::verify(verifying_key, &vrf_input, &output, &proof)
        .map_err(|e| format!("VRF verification failed: {}", e))
}

/// Calculate VRF score from VRF output
///
/// Extracts first 8 bytes of VRF output as big-endian u64.
/// This provides a score in range [0, 2^64-1] for chain comparison.
pub fn vrf_output_to_score(vrf_output: &Hash256) -> u64 {
    let output = ECVRFOutput::new(*vrf_output);
    output.as_u64()
}

/// Create VRF input from block context
///
/// VRF input must be deterministic and unpredictable before block production.
///
/// **Security Note:** We include previous_hash to provide unpredictable entropy.
/// This prevents VRF grinding attacks where an adversary with multiple masternodes
/// could pre-compute VRF outputs for future slots (since height/time are predictable).
///
/// The previous block hash changes with each block and cannot be predicted far in advance,
/// preventing attackers from selectively registering masternodes that will win specific future slots.
///
/// We use: SHA256("TIMECOIN_VRF_V2" || height || previous_hash)
fn create_vrf_input(height: u64, previous_hash: &Hash256) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"TIMECOIN_VRF_V2"); // Version bumped for security enhancement
    hasher.update(height.to_le_bytes());
    hasher.update(previous_hash); // Unpredictable entropy (changes each block)
    hasher.finalize().to_vec()
}

/// Fallback VRF generation when ECVRF fails
///
/// Uses deterministic hashing instead of cryptographic VRF.
/// This should never happen in production, but provides graceful degradation.
///
/// **Security:** Even in fallback mode, we maintain grinding resistance by
/// including the unpredictable previous_hash in the input.
fn fallback_vrf(height: u64, previous_hash: &Hash256) -> (Vec<u8>, Hash256, u64) {
    let mut hasher = Sha256::new();
    hasher.update(b"TIMECOIN_VRF_FALLBACK_V2"); // Version bumped
    hasher.update(height.to_le_bytes());
    hasher.update(previous_hash); // Unpredictable entropy
    let hash: [u8; 32] = hasher.finalize().into();

    let score = u64::from_be_bytes(hash[0..8].try_into().unwrap());

    // Empty proof indicates fallback was used
    (Vec::new(), hash, score)
}

/// Check if a VRF score qualifies this node as a block proposer.
///
/// Uses Algorand-style sortition: the threshold is set so that the expected
/// number of proposers per slot is TARGET_PROPOSERS (default 3). With multiple
/// eligible proposers, the lowest VRF score wins via best-proposal selection.
///
/// Higher TARGET_PROPOSERS = more reliable block production (fewer empty slots)
/// but slightly more network traffic from competing proposals.
pub fn vrf_check_proposer_eligible(
    vrf_score: u64,
    node_sampling_weight: u64,
    total_sampling_weight: u64,
) -> bool {
    if total_sampling_weight == 0 || node_sampling_weight == 0 {
        return false;
    }

    // Target ~3 proposers per slot on average for reliability
    // P(at least 1 proposer) = 1 - (1 - 3/N)^N ≈ 95% for N=6
    const TARGET_PROPOSERS: u128 = 3;

    // threshold = (node_weight / total_weight) * TARGET_PROPOSERS * u64::MAX
    // Capped at u64::MAX (100% probability)
    let threshold = ((node_sampling_weight as u128 * u64::MAX as u128 * TARGET_PROPOSERS)
        / total_sampling_weight as u128)
        .min(u64::MAX as u128);

    (vrf_score as u128) < threshold
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::RngCore;

    fn create_test_key() -> SigningKey {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn test_generate_block_vrf() {
        let sk = create_test_key();
        let height = 100;
        let prev_hash = [0u8; 32];

        let (proof, output, score) = generate_block_vrf(&sk, height, &prev_hash);

        // Proof should be 80 bytes (ECVRF standard)
        assert_eq!(proof.len(), 80);
        // Output should be 32 bytes
        assert_eq!(output.len(), 32);
        // Score should be non-zero with high probability
        assert!(score > 0);
    }

    #[test]
    fn test_vrf_deterministic() {
        let sk = create_test_key();
        let height = 100;
        let prev_hash = [0u8; 32];

        let (proof1, output1, score1) = generate_block_vrf(&sk, height, &prev_hash);
        let (proof2, output2, score2) = generate_block_vrf(&sk, height, &prev_hash);

        // Same input should produce same output
        assert_eq!(proof1, proof2);
        assert_eq!(output1, output2);
        assert_eq!(score1, score2);
    }

    #[test]
    fn test_vrf_different_heights() {
        let sk = create_test_key();
        let prev_hash = [0u8; 32];

        let (_, output1, score1) = generate_block_vrf(&sk, 100, &prev_hash);
        let (_, output2, score2) = generate_block_vrf(&sk, 101, &prev_hash);

        // Different heights should produce different outputs
        assert_ne!(output1, output2);
        assert_ne!(score1, score2);
    }

    #[test]
    fn test_vrf_different_prev_hash() {
        let sk = create_test_key();
        let height = 100;

        let (_, output1, score1) = generate_block_vrf(&sk, height, &[0u8; 32]);
        let (_, output2, score2) = generate_block_vrf(&sk, height, &[1u8; 32]);

        // Different previous hashes should produce different outputs
        assert_ne!(output1, output2);
        assert_ne!(score1, score2);
    }

    #[test]
    fn test_verify_valid_vrf() {
        let sk = create_test_key();
        let pk = sk.verifying_key();
        let height = 100;
        let prev_hash = [0u8; 32];

        let (proof, output, _score) = generate_block_vrf(&sk, height, &prev_hash);

        // Verification should succeed
        let result = verify_block_vrf(&pk, height, &prev_hash, &proof, &output);
        assert!(result.is_ok(), "Verification failed: {:?}", result);
    }

    #[test]
    fn test_verify_fails_wrong_height() {
        let sk = create_test_key();
        let pk = sk.verifying_key();
        let height = 100;
        let prev_hash = [0u8; 32];

        let (proof, output, _score) = generate_block_vrf(&sk, height, &prev_hash);

        // Verification should fail with different height
        let result = verify_block_vrf(&pk, 101, &prev_hash, &proof, &output);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_fails_wrong_prev_hash() {
        let sk = create_test_key();
        let pk = sk.verifying_key();
        let height = 100;
        let prev_hash = [0u8; 32];

        let (proof, output, _score) = generate_block_vrf(&sk, height, &prev_hash);

        // Verification should fail with different previous hash
        let result = verify_block_vrf(&pk, height, &[1u8; 32], &proof, &output);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_empty_proof_allowed() {
        let sk = create_test_key();
        let pk = sk.verifying_key();

        // Empty proof (old blocks before VRF) should be accepted
        let result = verify_block_vrf(&pk, 100, &[0u8; 32], &[], &[0u8; 32]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_vrf_output_to_score() {
        let output1 = [0xFF; 32]; // All ones
        let output2 = [0x00; 32]; // All zeros

        let score1 = vrf_output_to_score(&output1);
        let score2 = vrf_output_to_score(&output2);

        // All ones should give higher score than all zeros
        assert!(score1 > score2);
        assert_eq!(score2, 0);
    }

    #[test]
    fn test_fallback_vrf() {
        let height = 100;
        let prev_hash = [0u8; 32];

        let (proof, output, _score) = fallback_vrf(height, &prev_hash);

        // Fallback has empty proof
        assert!(proof.is_empty());
        // But still produces output and score
        assert_eq!(output.len(), 32);
        // Score is a valid u64 (any value is fine)
    }

    #[test]
    fn test_create_vrf_input() {
        let input1 = create_vrf_input(100, &[0u8; 32]);
        let input2 = create_vrf_input(100, &[0u8; 32]);
        let input3 = create_vrf_input(101, &[0u8; 32]);

        // Same parameters produce same input
        assert_eq!(input1, input2);
        // Different parameters produce different input
        assert_ne!(input1, input3);
    }
}
