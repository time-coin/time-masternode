//! Global constants for the TimeCoin blockchain
//!
//! Centralizes all magic numbers and configuration constants to improve
//! code maintainability and readability.

/// Blockchain protocol constants
pub mod blockchain {
    /// Target block time in seconds (10 minutes)
    pub const BLOCK_TIME_SECONDS: i64 = 600;

    /// Block reward in satoshis (100 TIME)
    pub const SATOSHIS_PER_TIME: u64 = 100_000_000;
    pub const BLOCK_REWARD_SATOSHIS: u64 = 100 * SATOSHIS_PER_TIME;

    /// Reward split: 30% leader bonus + 5% treasury + 65% per-tier pools (§10.4)
    pub const PRODUCER_REWARD_SATOSHIS: u64 = 30 * SATOSHIS_PER_TIME; // 30 TIME leader bonus

    /// Treasury allocation per block — deposited as on-chain state, not a UTXO
    pub const TREASURY_POOL_SATOSHIS: u64 = 5 * SATOSHIS_PER_TIME; // 5 TIME

    /// Per-tier reward pools — each tier's allocation rotates among its active nodes
    pub const GOLD_POOL_SATOSHIS: u64 = 25 * SATOSHIS_PER_TIME; // 25 TIME
    pub const SILVER_POOL_SATOSHIS: u64 = 18 * SATOSHIS_PER_TIME; // 18 TIME
    pub const BRONZE_POOL_SATOSHIS: u64 = 14 * SATOSHIS_PER_TIME; // 14 TIME
    pub const FREE_POOL_SATOSHIS: u64 = 8 * SATOSHIS_PER_TIME; //  8 TIME
    /// Total pool = 65 TIME (must equal BLOCK_REWARD - PRODUCER_REWARD - TREASURY_POOL)
    pub const TOTAL_POOL_SATOSHIS: u64 =
        GOLD_POOL_SATOSHIS + SILVER_POOL_SATOSHIS + BRONZE_POOL_SATOSHIS + FREE_POOL_SATOSHIS;

    pub const MIN_POOL_PAYOUT_SATOSHIS: u64 = SATOSHIS_PER_TIME; // 1 TIME minimum per recipient
    pub const MAX_TIER_RECIPIENTS: usize = 25; // Max recipients per tier per block

    /// Anti-sybil maturity gate: Free nodes must be online this many blocks before
    /// becoming eligible for VRF sortition and the participation pool.
    /// Only enforced on mainnet (testnet = 0 for rapid development iteration).
    pub const FREE_MATURITY_BLOCKS: u64 = 72; // ~12 hours at 10 min/block

    /// Maximum block size in bytes (1 MB)
    pub const MAX_BLOCK_SIZE: usize = 1_000_000;

    /// Maximum timestamp tolerance for future blocks (60 seconds for clock drift)
    /// CRITICAL: Must be << BLOCK_TIME_SECONDS (600s) to prevent accepting
    /// blocks that are ahead of schedule
    pub const TIMESTAMP_TOLERANCE_SECS: i64 = 60;

    /// Maximum depth for blockchain reorganization
    /// SECURITY: Blocks deeper than this are considered FINAL and cannot be reorged
    /// This protects against long-range attacks where an attacker creates a fake longer chain
    /// 6 blocks = 1 hour of blocks (at 10 min/block) - provides reasonable finality
    pub const MAX_REORG_DEPTH: u64 = 100;

    /// Maximum depth to search for common ancestor in fork resolution
    pub const MAX_FORK_SEARCH_DEPTH: u64 = 2_000;

    /// Blocks can be at most this far ahead without triggering full sync
    pub const MAX_SEQUENTIAL_GAP: u64 = 1;

    /// Trigger sync if we're this many blocks behind consensus
    pub const SYNC_TRIGGER_THRESHOLD: u64 = 10;

    /// Number of recent blocks to cache in memory
    pub const BLOCK_CACHE_SIZE: usize = 100;
}

/// Network protocol constants
pub mod network {
    /// Interval between ping messages (30 seconds)
    pub const PING_INTERVAL_SECS: u64 = 30;

    /// Timeout for waiting for pong response (90 seconds)
    pub const PONG_TIMEOUT_SECS: u64 = 90;

    /// Extended timeout for whitelisted peers (3 minutes)
    pub const WHITELISTED_PONG_TIMEOUT_SECS: u64 = 180;

    /// Maximum number of missed pongs before disconnection
    pub const MAX_MISSED_PONGS: u32 = 3;

    /// Maximum invalid blocks from a peer before disconnection
    pub const MAX_INVALID_BLOCKS: u32 = 5;

    /// Batch size for block synchronization
    pub const SYNC_BATCH_SIZE: u64 = 100;

    /// Pipeline depth for concurrent block requests
    pub const SYNC_PIPELINE_DEPTH: usize = 3;

    /// Default buffer size for message reading (2 MB)
    pub const MESSAGE_BUFFER_SIZE: usize = 2 * 1024 * 1024;
}

/// Fork resolution constants
pub mod fork_resolution {
    use std::time::Duration;

    /// Maximum time to spend searching for common ancestor
    pub const MAX_ANCESTOR_SEARCH_TIME: Duration = Duration::from_secs(120);

    /// Maximum time to spend fetching fork chain
    pub const MAX_FORK_FETCH_TIME: Duration = Duration::from_secs(300);

    /// Maximum time to spend performing reorganization
    pub const MAX_REORG_TIME: Duration = Duration::from_secs(60);
}

/// Performance tuning constants
pub mod performance {
    /// Number of blocks to process in parallel during validation
    pub const PARALLEL_VALIDATION_BATCH: usize = 10;

    /// Number of database operations to batch together
    pub const DB_BATCH_SIZE: usize = 100;
}
