//! Blockchain-specific error types
//!
//! This module provides strongly-typed errors for blockchain operations,
//! replacing String-based errors for better error handling and debugging.

use thiserror::Error;

/// Errors that can occur during blockchain operations
#[derive(Error, Debug)]
pub enum BlockchainError {
    /// Block not found at the specified height
    #[error("Block not found at height {0}")]
    BlockNotFound(u64),

    /// Invalid block structure or data
    #[error("Invalid block at height {height}: {reason}")]
    InvalidBlock { height: u64, reason: String },

    /// Block validation failed
    #[error("Block validation failed: {0}")]
    ValidationFailed(String),

    /// Merkle root mismatch
    #[error("Merkle root mismatch in block {height}: expected {expected:?}, got {actual:?}")]
    MerkleRootMismatch {
        height: u64,
        expected: [u8; 32],
        actual: [u8; 32],
    },

    /// Block timestamp is invalid
    #[error("Invalid timestamp in block {height}: {reason}")]
    InvalidTimestamp { height: u64, reason: String },

    /// Block size exceeds maximum allowed
    #[error("Block size {size} exceeds maximum {max} at height {height}")]
    BlockTooLarge {
        height: u64,
        size: usize,
        max: usize,
    },

    /// Previous hash mismatch
    #[error("Previous hash mismatch in block {height}")]
    PreviousHashMismatch { height: u64 },

    /// Fork detected at specified height
    #[error("Fork detected at height {0}")]
    ForkDetected(u64),

    /// Reorganization failed
    #[error("Reorganization failed at height {height}: {reason}")]
    ReorgFailed { height: u64, reason: String },

    /// Checkpoint validation failed
    #[error("Checkpoint validation failed at height {height}: expected {expected}, got {actual}")]
    CheckpointMismatch {
        height: u64,
        expected: String,
        actual: String,
    },

    /// Maximum reorganization depth exceeded
    #[error("Cannot reorganize beyond maximum depth {max}: attempted depth {attempted}")]
    ReorgTooDeep { max: u64, attempted: u64 },

    /// Storage/database error
    #[error("Storage error: {0}")]
    Storage(#[from] sled::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] bincode::Error),

    /// UTXO error
    #[error("UTXO error: {0}")]
    Utxo(String),

    /// Consensus error
    #[error("Consensus error: {0}")]
    Consensus(String),

    /// Sync error
    #[error("Sync error: {0}")]
    Sync(String),

    /// Genesis block error
    #[error("Genesis block error: {0}")]
    Genesis(String),

    /// Block production error
    #[error("Block production failed: {0}")]
    BlockProduction(String),

    /// Network error during sync
    #[error("Network error: {0}")]
    Network(String),

    /// No peers available for sync
    #[error("No peers available for synchronization")]
    NoPeersAvailable,

    /// Chain is incomplete (missing blocks)
    #[error("Chain incomplete: missing {count} blocks")]
    IncompleteChain { count: usize },

    /// Already syncing
    #[error("Sync already in progress")]
    AlreadySyncing,

    /// Operation timed out
    #[error("Operation timed out: {0}")]
    Timeout(String),

    /// Generic blockchain error for backwards compatibility
    #[error("{0}")]
    Other(String),
}

/// Result type for blockchain operations
pub type BlockchainResult<T> = Result<T, BlockchainError>;

impl BlockchainError {
    /// Check if this error is recoverable
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            BlockchainError::NoPeersAvailable
                | BlockchainError::AlreadySyncing
                | BlockchainError::Timeout(_)
                | BlockchainError::Network(_)
                | BlockchainError::Sync(_)
        )
    }

    /// Check if this error indicates a fork
    pub fn is_fork(&self) -> bool {
        matches!(self, BlockchainError::ForkDetected(_))
    }

    /// Check if this error is related to storage
    pub fn is_storage_error(&self) -> bool {
        matches!(
            self,
            BlockchainError::Storage(_) | BlockchainError::Serialization(_)
        )
    }
}

/// Convert String errors to BlockchainError for backwards compatibility
impl From<String> for BlockchainError {
    fn from(s: String) -> Self {
        // Try to parse common error patterns
        if s.contains("Block") && s.contains("not found") {
            // Try to extract height
            if let Some(height_str) = s.split_whitespace().find_map(|w| w.parse::<u64>().ok()) {
                return BlockchainError::BlockNotFound(height_str);
            }
        }

        if s.contains("Fork detected") || s.contains("fork") {
            // Try to extract height
            if let Some(height_str) = s.split_whitespace().find_map(|w| w.parse::<u64>().ok()) {
                return BlockchainError::ForkDetected(height_str);
            }
        }

        if s.contains("No peers") || s.contains("no peers") {
            return BlockchainError::NoPeersAvailable;
        }

        if s.contains("already syncing") || s.contains("Sync already") {
            return BlockchainError::AlreadySyncing;
        }

        // Default to Other
        BlockchainError::Other(s)
    }
}

/// Convert &str errors to BlockchainError
impl From<&str> for BlockchainError {
    fn from(s: &str) -> Self {
        BlockchainError::from(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        assert!(BlockchainError::NoPeersAvailable.is_recoverable());
        assert!(BlockchainError::ForkDetected(100).is_fork());

        // Test storage error classification
        let storage_err = BlockchainError::Storage(sled::Error::Io(std::io::Error::other("test")));
        assert!(storage_err.is_storage_error());
    }

    #[test]
    fn test_string_conversion() {
        let err: BlockchainError = "Block 123 not found".to_string().into();
        assert!(matches!(err, BlockchainError::BlockNotFound(123)));

        let err: BlockchainError = "Fork detected at height 456".to_string().into();
        assert!(matches!(err, BlockchainError::ForkDetected(456)));

        let err: BlockchainError = "No peers available".to_string().into();
        assert!(matches!(err, BlockchainError::NoPeersAvailable));
    }
}
