//! Global constants for the TimeCoin blockchain
//!
//! Centralizes all magic numbers and configuration constants to improve
//! code maintainability and readability.

/// Genesis block checkpoints — hardcoded hashes like Bitcoin's checkpoints.
/// Nodes reject any genesis block whose hash doesn't match these values.
pub mod genesis {
    /// Testnet genesis block hash — locked in 2026-03-07 with 7-node testnet
    pub const TESTNET_GENESIS_HASH: Option<&str> =
        Some("866273966c0f380e3f71360d4cd59933f2e8c936b02f4c2774b9fd0e913f0ebb");

    /// Mainnet genesis block hash — treasury-only genesis (v1.4.9)
    /// 100 TIME → treasury pool; masternodes first rewarded in block 1
    pub const MAINNET_GENESIS_HASH: Option<&str> =
        Some("84ef74a8860ef3540e52b2bc30f74c6b0cd22a3822286e4ec4fcaf1e3c60c0d1");
}

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

    /// Per-tier reward pools — paid tiers award their full pool to a single winner
    /// selected by fairness bonus; Free tier shares among up to MAX_FREE_TIER_RECIPIENTS.
    pub const GOLD_POOL_SATOSHIS: u64 = 25 * SATOSHIS_PER_TIME; // 25 TIME
    pub const SILVER_POOL_SATOSHIS: u64 = 18 * SATOSHIS_PER_TIME; // 18 TIME
    pub const BRONZE_POOL_SATOSHIS: u64 = 14 * SATOSHIS_PER_TIME; // 14 TIME
    pub const FREE_POOL_SATOSHIS: u64 = 8 * SATOSHIS_PER_TIME; //  8 TIME
    /// Total pool = 65 TIME (must equal BLOCK_REWARD - PRODUCER_REWARD - TREASURY_POOL)
    pub const TOTAL_POOL_SATOSHIS: u64 =
        GOLD_POOL_SATOSHIS + SILVER_POOL_SATOSHIS + BRONZE_POOL_SATOSHIS + FREE_POOL_SATOSHIS;

    pub const MIN_POOL_PAYOUT_SATOSHIS: u64 = SATOSHIS_PER_TIME; // 1 TIME minimum per recipient
    pub const MAX_FREE_TIER_RECIPIENTS: usize = 25; // Max recipients for Free tier per block

    /// Anti-sybil maturity gate: Free nodes must be online this many blocks before
    /// becoming eligible for VRF sortition and the participation pool.
    /// Only enforced on mainnet (testnet = 0 for rapid development iteration).
    pub const FREE_MATURITY_BLOCKS: u64 = 72; // ~12 hours at 10 min/block

    /// Maximum block size for validation — blocks from peers up to this size are accepted.
    /// Set to 4 MB to accommodate blocks produced before the assembly cap was enforced.
    pub const MAX_BLOCK_SIZE: usize = 4_000_000;

    /// Maximum block size when assembling a new block to broadcast.
    /// Kept below MAX_BLOCK_SIZE to leave room for serialization variance and overhead.
    /// This is enforced in the block producer; peers may still send larger legacy blocks.
    pub const MAX_BLOCK_ASSEMBLY_SIZE: usize = 1_900_000;

    /// Maximum timestamp tolerance for future blocks (5 seconds for clock drift)
    /// Blocks should be produced on time — this only covers minor NTP drift.
    pub const TIMESTAMP_TOLERANCE_SECS: i64 = 5;

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

    /// Pool distribution validation is skipped for blocks at or below this height.
    ///
    /// **Blocks 676–681** (2026-04-05): consensus split caused by non-deterministic
    /// `tier_for_wallet` when an operator shared the same wallet address across Silver
    /// and Free tier registrations. Fixed by deterministic tier logic (commit 8d2086a).
    ///
    /// **Block 1737** (2026-04-13): free-tier fairness violation (AV35) — a modified
    /// producer paid only 1 of 5 equally-deserving free-tier nodes (counter≥1000 for
    /// all candidates). The Check B fairness guard (added in the same release as
    /// FAIRNESS_V2_HEIGHT) correctly rejects block 1737, but it was deployed while that
    /// block was already accepted by a minority of pre-upgrade peers. Raising this
    /// constant to 1737 lets honest nodes at height 1736 accept block 1737 and re-join
    /// the majority chain. All blocks at height ≥ 1738 are fully validated.
    pub const POOL_VALIDATION_MIN_HEIGHT: u64 = 1737;

    /// Fork height at which collateral reward-address enforcement activates.
    ///
    /// After this height, a paid-tier masternode's reward is redirected to the
    /// collateral UTXO's output address when the registered reward_address differs.
    /// This eliminates the economic incentive for collateral squatting: a node
    /// that gossip-squats a UTXO it doesn't own cannot redirect rewards to itself.
    /// Free-tier nodes (no collateral) are unaffected.
    pub const COLLATERAL_REWARD_ENFORCEMENT_HEIGHT: u64 = 750;

    /// Fork height at which free-tier reward eligibility switches from gossip-based
    /// to on-chain registration.
    ///
    /// Before this height: free-tier eligibility is determined by gossip (who is
    /// currently connected), which can be gamed by a VRF leader disconnecting other
    /// free nodes before producing a block (AV35 / targeted-disconnect attack).
    ///
    /// At and after this height: only nodes that have submitted a `FreeNodeRegistration`
    /// special transaction AND waited FREE_MATURITY_BLOCKS are eligible for free-tier
    /// rewards.  The eligible set is computed deterministically from on-chain state,
    /// so producer and validator always agree — the attack surface collapses to zero.
    ///
    /// Operators running free-tier nodes must submit a `freetierregister` transaction
    /// before this height and wait for maturity.  The daemon auto-submits on startup
    /// if `masternode=1` and no collateral is configured.
    pub const FREE_TIER_ONCHAIN_HEIGHT: u64 = 2160;

    /// Minimum transaction fee for a FreeNodeRegistration special transaction.
    /// Creates a small economic barrier against spam-registering many fake nodes.
    /// Fee is collected by the block producer as normal; nothing is burned.
    pub const FREE_TIER_REG_FEE_SATOSHIS: u64 = 100_000_000; // 1 TIME

    /// Fork height at which the improved fairness-rotation formula activates (v2).
    ///
    /// Before this height: fairness_bonus = blocks_without_reward / 10
    ///   → nodes paid 0–9 blocks ago all tie (bonus=0); the alphabetically-first IP
    ///     wins every tiebreak, allowing the same node to win dozens of consecutive
    ///     times when all nodes have blocks_without_reward < 10.
    ///
    /// At and after this height: fairness_bonus = blocks_without_reward (direct)
    ///   → a node paid 1 block ago has bonus=1, a node paid 5 blocks ago has bonus=5.
    ///     The highest-waiting node wins; recently-paid nodes go to the back of the
    ///     queue immediately instead of only after 10 blocks.
    ///
    /// Both the block producer and the validator must apply the same formula.
    /// All mainnet nodes must upgrade before this height to avoid a consensus split.
    pub const FAIRNESS_V2_HEIGHT: u64 = 1730;
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

    /// Batch size for block synchronization requests
    pub const SYNC_BATCH_SIZE: u64 = 100;

    /// Max blocks per response to avoid exceeding 8MB frame limit.
    /// At ~100-150KB per block, 50 blocks ≈ 5-7.5MB (under 8MB).
    pub const MAX_BLOCKS_PER_RESPONSE: u64 = 50;

    /// Batch size for fork resolution block requests (smaller to ensure delivery)
    pub const FORK_RESOLUTION_BATCH_SIZE: u64 = 20;

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

/// Masternode authority constants
pub mod masternode_authority {
    /// Number of reward violations before slashing collateral
    pub const REWARD_VIOLATION_THRESHOLD: u64 = 3;
}

/// Performance tuning constants
pub mod performance {
    /// Number of blocks to process in parallel during validation
    pub const PARALLEL_VALIDATION_BATCH: usize = 10;

    /// Number of database operations to batch together
    pub const DB_BATCH_SIZE: usize = 100;
}
