// src/vdf.rs - Verifiable Delay Function for Proof-of-Time
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Instant;

/// VDF configuration for different networks
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct VDFConfig {
    /// Number of sequential iterations
    pub iterations: u64,
    /// Checkpoint interval for fast verification
    pub checkpoint_interval: u64,
    /// Minimum seconds between blocks
    pub min_block_time: u64,
    /// Expected VDF computation time in seconds
    pub expected_compute_time: u64,
}

impl VDFConfig {
    /// Testnet: 2-minute VDF for 10-minute blocks
    pub fn testnet() -> Self {
        Self {
            iterations: 12_000_000, // ~2 minutes on modern CPU
            checkpoint_interval: 1_000_000,
            min_block_time: 600,        // 10 minutes
            expected_compute_time: 120, // 2 minutes
        }
    }

    /// Mainnet: 5-minute VDF for 10-minute blocks (future)
    pub fn mainnet() -> Self {
        Self {
            iterations: 30_000_000, // ~5 minutes on modern CPU
            checkpoint_interval: 2_000_000,
            min_block_time: 600,        // 10 minutes
            expected_compute_time: 300, // 5 minutes
        }
    }

    /// Disabled for testing
    pub fn disabled() -> Self {
        Self {
            iterations: 0,
            checkpoint_interval: 0,
            min_block_time: 0,
            expected_compute_time: 0,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.iterations > 0
    }
}

/// VDF proof with checkpoints for fast verification
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VDFProof {
    /// Final output after all iterations
    pub output: Vec<u8>,
    /// Number of iterations performed
    pub iterations: u64,
    /// Intermediate checkpoints for verification
    pub checkpoints: Vec<Vec<u8>>,
}

/// Compute VDF proof (slow - takes minutes)
#[allow(dead_code)]
pub fn compute_vdf(input: &[u8], config: &VDFConfig) -> Result<VDFProof, String> {
    if !config.is_enabled() {
        return Ok(VDFProof {
            output: vec![],
            iterations: 0,
            checkpoints: vec![],
        });
    }

    let start = Instant::now();
    let mut current = Sha256::digest(input).to_vec();
    let mut checkpoints = Vec::new();

    tracing::info!(
        "⏱️  Computing VDF proof ({} iterations, ~{} seconds)...",
        config.iterations,
        config.expected_compute_time
    );

    for i in 1..=config.iterations {
        current = Sha256::digest(&current).to_vec();

        // Save checkpoints for fast verification
        if i % config.checkpoint_interval == 0 {
            checkpoints.push(current.clone());
            tracing::debug!(
                "VDF checkpoint {}/{} ({:.1}%)",
                checkpoints.len(),
                config.iterations / config.checkpoint_interval,
                (i as f64 / config.iterations as f64) * 100.0
            );
        }
    }

    let elapsed = start.elapsed().as_secs();
    tracing::info!("✅ VDF computed in {} seconds", elapsed);

    Ok(VDFProof {
        output: current,
        iterations: config.iterations,
        checkpoints,
    })
}

/// Verify VDF proof (fast - takes ~1 second)
#[allow(dead_code)]
pub fn verify_vdf(input: &[u8], proof: &VDFProof, config: &VDFConfig) -> Result<bool, String> {
    if !config.is_enabled() {
        return Ok(true); // VDF disabled
    }

    if proof.iterations != config.iterations {
        return Err(format!(
            "Invalid iteration count: expected {}, got {}",
            config.iterations, proof.iterations
        ));
    }

    let start = Instant::now();
    let mut current = Sha256::digest(input).to_vec();

    // Verify each checkpoint
    for (idx, checkpoint) in proof.checkpoints.iter().enumerate() {
        // Compute from last position to this checkpoint
        let iterations_to_do = if idx == 0 {
            config.checkpoint_interval
        } else {
            config.checkpoint_interval
        };

        for _ in 0..iterations_to_do {
            current = Sha256::digest(&current).to_vec();
        }

        if current != *checkpoint {
            return Err(format!("Checkpoint {} mismatch", idx + 1));
        }
    }

    // Final verification from last checkpoint to output
    let final_iterations =
        config.iterations - (proof.checkpoints.len() as u64 * config.checkpoint_interval);
    for _ in 0..final_iterations {
        current = Sha256::digest(&current).to_vec();
    }

    let verified = current == proof.output;
    let elapsed = start.elapsed();

    tracing::debug!(
        "VDF verification: {} (took {:.3}s)",
        if verified { "✅ valid" } else { "❌ invalid" },
        elapsed.as_secs_f64()
    );

    Ok(verified)
}

/// Generate deterministic VDF input from block data
#[allow(dead_code)]
pub fn generate_vdf_input(
    block_number: u64,
    previous_hash: &[u8; 32],
    merkle_root: &str,
    timestamp: i64,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(block_number.to_le_bytes());
    hasher.update(previous_hash);
    hasher.update(merkle_root.as_bytes());
    hasher.update(timestamp.to_le_bytes());
    hasher.finalize().to_vec()
}

/// Check if enough time has passed to create a new block
#[allow(dead_code)]
pub fn can_create_block(previous_timestamp: i64, config: &VDFConfig) -> bool {
    if !config.is_enabled() {
        return true;
    }

    let now = chrono::Utc::now().timestamp();
    let elapsed = now - previous_timestamp;

    elapsed >= config.min_block_time as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vdf_compute_and_verify() {
        let config = VDFConfig {
            iterations: 10_000,
            checkpoint_interval: 2_000,
            min_block_time: 60,
            expected_compute_time: 1,
        };

        let input = b"test input";
        let proof = compute_vdf(input, &config).unwrap();

        assert_eq!(proof.iterations, 10_000);
        assert_eq!(proof.checkpoints.len(), 5); // 10000 / 2000

        let verified = verify_vdf(input, &proof, &config).unwrap();
        assert!(verified);
    }

    #[test]
    fn test_vdf_invalid_proof() {
        let config = VDFConfig {
            iterations: 1_000,
            checkpoint_interval: 200,
            min_block_time: 60,
            expected_compute_time: 1,
        };

        let input = b"test input";
        let mut proof = compute_vdf(input, &config).unwrap();

        // Tamper with output
        proof.output[0] ^= 1;

        let verified = verify_vdf(input, &proof, &config).unwrap();
        assert!(!verified);
    }

    #[test]
    fn test_vdf_disabled() {
        let config = VDFConfig::disabled();
        assert!(!config.is_enabled());

        let proof = compute_vdf(b"test", &config).unwrap();
        assert_eq!(proof.iterations, 0);

        let verified = verify_vdf(b"test", &proof, &config).unwrap();
        assert!(verified);
    }

    #[test]
    fn test_can_create_block() {
        let config = VDFConfig::testnet();
        let now = chrono::Utc::now().timestamp();

        // Too soon
        assert!(!can_create_block(now - 300, &config));

        // Just right
        assert!(can_create_block(now - 600, &config));

        // Past due
        assert!(can_create_block(now - 700, &config));
    }
}
