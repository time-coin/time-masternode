use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use timed::http_client::HttpClient;

#[derive(Parser, Debug)]
#[command(name = "time-cli")]
#[command(about = "TIME Coin CLI - Bitcoin-like RPC client", long_about = None)]
#[command(help_template = "\
{before-help}{name} - {about}

{usage-heading} {usage}

Commands:
  Blockchain
    getblockchaininfo      Get blockchain information
    getblock               Get block by height or hash
    getblockcount          Get the current block count
    getbestblockhash       Get the hash of the best (tip) block
    getblockhash           Get block hash at a given height
    getblockheader         Get block header by height or hash
    gettxoutsetinfo        Get information about the UTXO set
    gettxout               Get details about an unspent transaction output [txid] [vout]
  Network
    getnetworkinfo         Get network information
    getpeerinfo            Get peer information
    getconnectioncount     Get number of active peer connections
    getwhitelist           List all whitelisted IPs
    addwhitelist           Add an IP to the whitelist
    removewhitelist        Remove an IP from the whitelist
    getblacklist           List all banned IPs with reasons
    ban                    Permanently ban an IP address
    unban                  Remove an IP from the ban list
    clearbanlist           Remove ALL bans
    auditcollateral        Scan for collateral squatters and evict them
    aggregateblacklists    Collect and merge ban lists from multiple nodes
  Wallet
    getbalance             Get wallet balance [address]
    getwalletinfo          Get wallet information
    getnewaddress          Get this node's reward address
    getaddressinfo         Get info about an address (ismine, pubkey, etc.)
    getaddresspubkey       Get the public key for an address
    listreceivedbyaddress  List addresses with received balances
    listunspent            List unspent transaction outputs
    listunspentmulti       List unspent UTXOs across multiple addresses
    listtransactions       List recent wallet transactions
    sendtoaddress          Send TIME to an address
    sendfrom               Send TIME from a specific address
    mergeutxos             Merge UTXOs to reduce UTXO set size
    signmessage            Sign a message with this node's wallet key
    verifymessage          Verify a signed message
  Payment Requests
    requestpayment         Create a payment request
    payrequest             Pay a payment request
    sendpaymentrequest     Send a payment request to a peer
    getpaymentrequests     List payment requests for an address
    respondpaymentrequest  Respond to a payment request
    cancelpaymentrequest   Cancel a payment request
    markpaymentrequestviewed  Mark a payment request as viewed
  Transaction
    gettransaction         Get information about a transaction
    getrawtransaction      Get raw transaction data
    createrawtransaction   Create a new transaction
    decoderawtransaction   Decode a raw transaction
    sendrawtransaction     Send a raw transaction
    gettransactionfinality Get finality status for a transaction
    estimatesmartfee       Estimate fee for a target confirmation speed
  Masternode
    masternodelist         List all masternodes
    masternodestatus       Get this node's masternode status
    masternoderegstatus    Get on-chain registration status of a masternode
    checkcollateral        Check collateral UTXO health (on-chain, lock, squatter, tier)
    findcollateral         Look up who is claiming any collateral outpoint [txid:vout]
    masternode genkey      Generate a new masternode key
    masternode list        List masternodes (alias)
    masternode status      Get masternode status (alias)
    masternodereg          Register/re-register masternode on-chain (cold wallet signs, operator key embedded)
    dumpprivkey            Export the Ed25519 private key for a wallet.dat (offline, no RPC needed)
    releaseallcollaterals  Release ALL collateral locks (safe recovery — does not touch tx locks)
    listlockedcollaterals  List all locked collateral UTXOs
  UTXO / Collateral
    listlockedutxos        List all locked UTXOs
    unlockutxo             Unlock a specific UTXO (txid vout)
    unlockcollateral       Unlock a stuck masternode collateral UTXO (txid vout)
    unlockorphanedutxos    Unlock UTXOs locked by non-existent transactions
    forceunlockall         Force-unlock ALL UTXOs (recovery only)
    clearstuktransactions  Clear stuck/pending transactions
    cleanuplockedutxos     Clean up stale UTXO locks
  Mempool
    getmempoolinfo         Get memory pool information
    getrawmempool          Get raw memory pool
    getmempoolverbose      Get detailed mempool transactions
  Consensus
    getconsensusinfo       Get consensus information
    gettimevotestatus      Get TimeVote consensus engine status
  Treasury
    gettreasurybalance     Get on-chain treasury balance
  Governance
    listproposals          List all governance proposals
    getproposal            Get a specific governance proposal
    submitproposal         Submit a new governance proposal
    voteproposal           Vote on a governance proposal
  Utility
    validateaddress        Validate a TIME address
    stop                   Stop the daemon
    uptime                 Get daemon uptime
    getinfo                Get general node information
    getfeeschedule         Get the current transaction fee schedule
    gettxindexstatus       Get transaction index rebuild status
    reindextransactions    Rebuild transaction index
    reindex                Full reindex: rebuild UTXOs + tx index from block 0
    rollbacktoblock0       [DANGER] Delete all blocks above genesis and reset chain
    rollbacktoheight       [DANGER] Roll back chain to a specific block height
    resetfinalitylock      [DANGER] Reset BFT finality lock to recover a stuck fork node

    help                   Print this message or the help of the given subcommand(s)

{options}{after-help}
")]
struct Args {
    /// RPC server address (overrides --testnet flag)
    #[arg(short, long)]
    rpc_url: Option<String>,

    /// Connect to testnet (port 24101 instead of mainnet 24001)
    #[arg(long)]
    testnet: bool,

    /// RPC username (reads from .cookie file if not provided)
    #[arg(long)]
    rpcuser: Option<String>,

    /// RPC password (reads from .cookie file if not provided)
    #[arg(long)]
    rpcpassword: Option<String>,

    /// Output compact JSON (single line)
    #[arg(long)]
    compact: bool,

    /// Output human-readable format
    #[arg(long)]
    human: bool,

    /// Skip TLS certificate verification (for self-signed RPC certs)
    #[arg(long)]
    no_tls_verify: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "lowercase")]
enum Commands {
    // ============================================================
    // BLOCKCHAIN COMMANDS
    // ============================================================
    /// Get blockchain information
    #[command(next_help_heading = "Blockchain")]
    GetBlockchainInfo,

    /// Get information about a specific block
    #[command(next_help_heading = "Blockchain")]
    GetBlock {
        /// Block height or hash
        height: u64,
    },

    /// Get the current block count
    #[command(next_help_heading = "Blockchain")]
    GetBlockCount,

    /// Get the hash of the best (tip) block
    #[command(next_help_heading = "Blockchain")]
    GetBestBlockHash,

    /// Get block hash at a given height
    #[command(next_help_heading = "Blockchain")]
    GetBlockHash {
        /// Block height
        height: u64,
    },

    /// Get block header information
    #[command(next_help_heading = "Blockchain")]
    GetBlockHeader {
        /// Block height or hash
        height_or_hash: String,
    },

    /// Get information about the UTXO set
    #[command(next_help_heading = "Blockchain")]
    GetTxOutSetInfo,

    /// Get details about an unspent transaction output
    #[command(next_help_heading = "Blockchain")]
    GetTxOut {
        /// Transaction ID
        txid: String,
        /// Output index
        vout: u32,
    },

    // ============================================================
    // NETWORK COMMANDS
    // ============================================================
    /// Get network information
    #[command(next_help_heading = "Network")]
    GetNetworkInfo,

    /// Get peer information
    #[command(next_help_heading = "Network")]
    GetPeerInfo,

    /// Scan all paid-tier masternode registrations for collateral squatters.
    /// Evicts squatters, releases their duplicate locks, and permanently bans them.
    #[command(next_help_heading = "Network")]
    AuditCollateral,

    /// List all banned IPs (permanent and temporary) with reasons
    #[command(next_help_heading = "Network")]
    GetBlacklist,

    /// Permanently ban an IP address
    #[command(next_help_heading = "Network")]
    Ban {
        /// IP address to ban (e.g. 47.82.79.157)
        ip: String,
        /// Optional reason for the ban
        reason: Option<String>,
    },

    /// Remove an IP from the ban list and clear its violations
    #[command(next_help_heading = "Network")]
    Unban {
        /// IP address to unban (e.g. 154.217.246.86)
        ip: String,
    },

    /// Remove ALL bans and violation counts (whitelisted peers are unaffected)
    #[command(next_help_heading = "Network")]
    ClearBanList,

    /// Add an IP to the whitelist (exempt from bans and rate limits; must be a registered network peer)
    #[command(next_help_heading = "Network")]
    AddWhitelist {
        /// IP address to whitelist (e.g. 69.167.168.176)
        ip: String,
    },

    /// List all whitelisted IPs
    #[command(next_help_heading = "Network")]
    GetWhitelist,

    /// Remove an IP from the whitelist
    #[command(next_help_heading = "Network")]
    RemoveWhitelist {
        /// IP address to remove from whitelist
        ip: String,
    },

    /// Get number of active peer connections
    #[command(next_help_heading = "Network")]
    GetConnectionCount,

    /// Collect and merge ban lists from multiple nodes.
    /// Queries each node's getblacklist RPC and produces a unified report
    /// showing which IPs are banned across the network and on how many nodes.
    ///
    /// Example:
    ///   time-cli aggregateblacklists http://1.2.3.4:24001 http://5.6.7.8:24001
    #[command(next_help_heading = "Network")]
    AggregateBlacklists {
        /// RPC URLs to query (e.g. http://1.2.3.4:24001). Queries localhost only if none given.
        nodes: Vec<String>,
    },

    // ============================================================
    // WALLET COMMANDS
    // ============================================================
    /// Get wallet balance
    #[command(next_help_heading = "Wallet")]
    GetBalance {
        /// Address to query (defaults to this node's reward address)
        address: Option<String>,
    },

    /// Get wallet information
    #[command(next_help_heading = "Wallet")]
    GetWalletInfo,

    /// Get a new receiving address
    #[command(next_help_heading = "Wallet")]
    GetNewAddress,

    /// List addresses with balances
    #[command(next_help_heading = "Wallet")]
    ListReceivedByAddress {
        /// Minimum confirmations (default: 1)
        #[arg(short, long, default_value = "1")]
        minconf: u32,
        /// Include addresses with zero balance
        #[arg(short = 'z', long)]
        include_empty: bool,
    },

    /// List unspent transaction outputs
    #[command(next_help_heading = "Wallet")]
    ListUnspent {
        /// Number of UTXOs to display (default: 10, use 0 for all)
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Minimum confirmations
        #[arg(default_value = "1")]
        minconf: u32,
        /// Maximum confirmations
        #[arg(default_value = "9999999")]
        maxconf: u32,
    },

    /// List recent wallet transactions (sent and received)
    #[command(next_help_heading = "Wallet")]
    ListTransactions {
        /// Number of transactions to show (default: 10)
        #[arg(short = 'n', long, default_value = "10")]
        count: u64,
    },

    /// Get info about an address (ismine, pubkey, type)
    #[command(next_help_heading = "Wallet")]
    GetAddressInfo {
        /// TIME address to look up
        address: String,
    },

    /// List unspent UTXOs across multiple addresses
    #[command(next_help_heading = "Wallet")]
    ListUnspentMulti {
        /// JSON array of addresses (e.g. '["addr1","addr2"]')
        addresses: String,
    },

    /// Sign a message with this node's wallet key
    #[command(next_help_heading = "Wallet")]
    SignMessage {
        /// Message to sign
        message: String,
        /// Address to sign with (uses default wallet address if not specified)
        address: Option<String>,
    },

    /// Verify a message signature
    #[command(next_help_heading = "Wallet")]
    VerifyMessage {
        /// Address that signed the message
        address: String,
        /// Signature (hex)
        signature: String,
        /// Original message
        message: String,
    },

    /// Send TIME to an address
    #[command(next_help_heading = "Wallet")]
    SendToAddress {
        /// Recipient address
        address: String,
        /// Amount to send (in TIME)
        amount: f64,
        /// Subtract fee from amount (recipient gets amount minus fee)
        #[arg(long, default_value = "false")]
        subtract_fee: bool,
        /// Return TXID immediately without waiting for finality
        #[arg(long, default_value = "false")]
        nowait: bool,
        /// Encrypted memo (only sender and recipient can read it)
        #[arg(long)]
        memo: Option<String>,
    },

    /// Send TIME from a specific address
    #[command(next_help_heading = "Wallet")]
    SendFrom {
        /// Source address to spend UTXOs from
        from_address: String,
        /// Recipient address
        to_address: String,
        /// Amount to send (in TIME)
        amount: f64,
        /// Subtract fee from amount (recipient gets amount minus fee)
        #[arg(long, default_value = "false")]
        subtract_fee: bool,
        /// Return TXID immediately without waiting for finality
        #[arg(long, default_value = "false")]
        nowait: bool,
    },

    /// Merge UTXOs to reduce UTXO set size
    #[command(next_help_heading = "Wallet")]
    MergeUtxos {
        /// Minimum number of UTXOs required to merge (default: 2)
        #[arg(short, long, default_value = "2")]
        min_count: usize,
        /// Maximum number of UTXOs to merge in one transaction (default: 100)
        #[arg(short = 'x', long, default_value = "100")]
        max_count: usize,
        /// Address to merge UTXOs for (optional, uses default wallet if not specified)
        #[arg(short, long)]
        address: Option<String>,
    },

    /// Create a payment request URI to share with a payer
    #[command(next_help_heading = "Wallet")]
    RequestPayment {
        /// Amount to request (in TIME)
        amount: f64,
        /// Description / memo for the payment
        #[arg(long)]
        memo: Option<String>,
        /// Label for the requester (e.g. merchant name)
        #[arg(long)]
        label: Option<String>,
    },

    /// Pay a payment request URI
    #[command(next_help_heading = "Wallet")]
    PayRequest {
        /// Payment request URI (timecoin:ADDRESS?amount=X&pubkey=HEX&memo=TEXT)
        uri: String,
        /// Override the memo with a custom message
        #[arg(long)]
        memo: Option<String>,
    },

    /// Send a P2P payment request to another address
    #[command(next_help_heading = "Wallet")]
    SendPaymentRequest {
        /// Your address (requester)
        from_address: String,
        /// Recipient address (payer)
        to_address: String,
        /// Amount requested in TIME atoms
        amount: u64,
        /// Description / memo for the payment
        #[arg(long)]
        memo: Option<String>,
        /// Your Ed25519 pubkey (hex) for the recipient to encrypt the payment memo
        #[arg(long)]
        pubkey: String,
        /// Ed25519 signature over the request fields (hex)
        #[arg(long)]
        signature: String,
        /// Unix timestamp of the request
        #[arg(long)]
        timestamp: i64,
        /// Optional display name for the requester
        #[arg(long)]
        requester_name: Option<String>,
    },

    /// List incoming payment requests (as payer)
    #[command(next_help_heading = "Wallet")]
    GetPaymentRequests {
        /// Filter by payer address (your address)
        address: Option<String>,
    },

    /// Respond to a payment request (accept or decline)
    #[command(next_help_heading = "Wallet")]
    RespondPaymentRequest {
        /// Payment request ID
        request_id: String,
        /// Address of the requester
        requester_address: String,
        /// Your payer address
        payer_address: String,
        /// Accept (true) or decline (false)
        accepted: bool,
        /// Transaction ID if accepted
        #[arg(long)]
        txid: Option<String>,
    },

    /// Cancel a pending payment request you sent
    #[command(next_help_heading = "Wallet")]
    CancelPaymentRequest {
        /// Payment request ID
        request_id: String,
        /// Your requester address
        requester_address: String,
    },

    /// Mark a payment request as viewed (notifies the requester)
    #[command(next_help_heading = "Wallet")]
    MarkPaymentRequestViewed {
        /// Payment request ID
        request_id: String,
        /// Address of the requester
        requester_address: String,
        /// Your payer address
        payer_address: String,
    },

    // ============================================================
    // TRANSACTION COMMANDS
    // ============================================================
    /// Get information about a transaction
    #[command(next_help_heading = "Transaction")]
    GetTransaction {
        /// Transaction ID (hex)
        txid: String,
    },

    /// Get raw transaction data
    #[command(next_help_heading = "Transaction")]
    GetRawTransaction {
        /// Transaction ID (hex)
        txid: String,
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// Create a new transaction
    #[command(next_help_heading = "Transaction")]
    CreateRawTransaction {
        /// JSON array of inputs
        inputs: String,
        /// JSON object of outputs
        outputs: String,
    },

    /// Decode a raw transaction
    #[command(next_help_heading = "Transaction")]
    DecodeRawTransaction {
        /// Hex-encoded transaction
        hex: String,
    },

    /// Send a raw transaction
    #[command(next_help_heading = "Transaction")]
    SendRawTransaction {
        /// Hex-encoded transaction
        hex: String,
    },

    /// Get finality status for a transaction
    #[command(next_help_heading = "Transaction")]
    GetTransactionFinality {
        /// Transaction ID (hex)
        txid: String,
    },

    /// Estimate the fee required for a target confirmation speed
    #[command(next_help_heading = "Transaction")]
    EstimateSmartFee {
        /// Target blocks for confirmation (default: 1)
        #[arg(default_value = "1")]
        conf_target: u32,
    },

    // ============================================================
    // MASTERNODE COMMANDS
    // ============================================================
    /// Masternode commands (genkey, list, status)
    #[command(next_help_heading = "Masternode")]
    Masternode {
        #[command(subcommand)]
        subcmd: MasternodeCommands,
    },

    /// Get masternode information (connected only by default) [alias for: masternode list]
    #[command(next_help_heading = "Masternode")]
    MasternodeList {
        /// Show all masternodes including disconnected
        #[arg(long)]
        all: bool,
    },

    /// Get masternode status [alias for: masternode status]
    #[command(next_help_heading = "Masternode")]
    MasternodeStatus,

    /// Release ALL collateral locks without touching transaction UTXO locks.
    /// Use when squatters have locked your collateral UTXOs and you need to reclaim them.
    /// After running this, restart the node so legitimate masternodes re-register.
    #[command(next_help_heading = "Masternode")]
    ReleaseAllCollaterals,

    /// List all locked collaterals
    #[command(next_help_heading = "Masternode")]
    ListLockedCollaterals,

    /// Get on-chain registration status of a masternode
    #[command(next_help_heading = "Masternode")]
    MasternodeRegStatus {
        /// Node address IP:port to check (uses this node's address if not specified)
        address: Option<String>,
    },

    /// Check the health of the local masternode's configured collateral UTXO.
    ///
    /// Reports whether the UTXO exists on-chain, the detected tier, lock status,
    /// and whether a squatter has claimed the outpoint in the gossip registry.
    #[command(next_help_heading = "Masternode")]
    CheckCollateral,

    /// Look up who is currently claiming any collateral outpoint.
    ///
    /// Works for any txid:vout, including UTXOs not configured locally.
    /// Use this to diagnose squatted collaterals across all your masternodes.
    ///
    /// Example:
    ///   time-cli findcollateral abc123...:0
    #[command(next_help_heading = "Masternode")]
    FindCollateral {
        /// Outpoint in the form txid:vout (e.g. abc123...:0)
        outpoint: String,
    },

    /// Register or re-register a masternode on-chain (two-key model).
    ///
    /// Signs the MasternodeReg transaction with the wallet that owns the collateral UTXO,
    /// embedding the masternode node's operator public key so the running node can be
    /// verified without the cold wallet ever going online again.
    ///
    /// Example:
    ///   time-cli masternodereg \
    ///     --collateral abc123...:0 \
    ///     --masternode-ip 50.28.104.50 \
    ///     --payout-address T... \
    ///     --operator-pubkey <hex from `masternode genkey` or `masternodestatus`> \
    ///     --wallet-path /path/to/wallet.dat \
    ///     --wallet-password mypassword
    #[command(next_help_heading = "Masternode")]
    MasternodeReg {
        /// Collateral outpoint in txid:vout format (e.g. abc123...:0)
        #[arg(long)]
        collateral: String,
        /// Masternode public IP address
        #[arg(long)]
        masternode_ip: String,
        /// P2P port (default: 24000 mainnet / 24100 testnet)
        #[arg(long)]
        port: Option<u16>,
        /// TIME address that will receive block rewards (your GUI wallet address)
        #[arg(long)]
        payout_address: String,
        /// Path to the wallet file that owns the collateral UTXO.
        /// Defaults to the standard data-dir wallet.dat
        #[arg(long)]
        wallet_path: Option<String>,
        /// Wallet decryption password (leave empty for unencrypted wallets)
        #[arg(long, default_value = "")]
        wallet_password: String,
        /// Raw Ed25519 private key in hex (32 bytes = 64 hex chars).
        /// Use instead of --wallet-path when the cold wallet is on a separate machine.
        /// Obtain via: time-cli dumpprivkey --wallet-path /path/to/wallet.dat
        #[arg(long)]
        privkey: Option<String>,
    },

    /// Export the Ed25519 private key from a wallet.dat file.
    /// Works offline — no running daemon or RPC needed.
    /// Use this to obtain the --privkey value for masternodereg when your
    /// cold wallet is on a machine that does not run an RPC server.
    #[command(next_help_heading = "Masternode")]
    DumpPrivKey {
        /// Path to the wallet.dat file to export from
        #[arg(long)]
        wallet_path: Option<String>,
        /// Wallet decryption password (leave empty for unencrypted wallets)
        #[arg(long, default_value = "")]
        wallet_password: String,
    },

    // ============================================================
    // MEMPOOL COMMANDS
    // ============================================================
    /// Get memory pool information
    #[command(next_help_heading = "Mempool")]
    GetMempoolInfo,

    /// Get raw memory pool
    #[command(next_help_heading = "Mempool")]
    GetRawMempool {
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// Get detailed mempool transactions
    #[command(next_help_heading = "Mempool")]
    GetMempoolVerbose,

    // ============================================================
    // CONSENSUS COMMANDS
    // ============================================================
    /// Get consensus information
    #[command(next_help_heading = "Consensus")]
    GetConsensusInfo,

    /// Get TimeVote consensus engine status
    #[command(next_help_heading = "Consensus")]
    GetTimeVoteStatus,

    // ============================================================
    // TREASURY COMMANDS
    // ============================================================
    /// Get the on-chain treasury balance
    #[command(next_help_heading = "Treasury")]
    GetTreasuryBalance,

    // ============================================================
    // GOVERNANCE COMMANDS
    // ============================================================
    /// List all governance proposals
    #[command(next_help_heading = "Governance")]
    ListProposals,

    /// Get a specific governance proposal
    #[command(next_help_heading = "Governance")]
    GetProposal {
        /// Proposal ID
        proposal_id: String,
    },

    /// Submit a new governance proposal
    #[command(next_help_heading = "Governance")]
    SubmitProposal {
        /// Proposal type: treasury_spend, fee_schedule_change, emission_rate_change
        proposal_type: String,
        /// Proposal data as JSON string
        data: String,
    },

    /// Vote on a governance proposal
    #[command(next_help_heading = "Governance")]
    VoteProposal {
        /// Proposal ID
        proposal_id: String,
        /// Vote: yes or no
        vote: String,
    },

    // ============================================================
    // UTILITY COMMANDS
    // ============================================================
    /// Validate an address
    #[command(next_help_heading = "Utility")]
    ValidateAddress {
        /// Address to validate
        address: String,
    },

    /// Stop the daemon
    #[command(next_help_heading = "Utility")]
    Stop,

    /// Get daemon uptime
    #[command(next_help_heading = "Utility")]
    Uptime,

    /// Get general information about the node
    #[command(next_help_heading = "Utility")]
    GetInfo,

    /// Get the current transaction fee schedule
    #[command(next_help_heading = "Utility")]
    GetFeeSchedule,

    /// Get transaction index rebuild status
    #[command(next_help_heading = "Utility")]
    GetTxIndexStatus,

    /// Rebuild transaction index
    #[command(next_help_heading = "Utility")]
    ReindexTransactions,

    /// Full reindex: rebuild UTXOs and transaction index from block 0
    #[command(next_help_heading = "Utility")]
    Reindex,

    /// Delete all blocks above height 0 and reset chain to genesis (DANGER: wipes all post-genesis chain data)
    #[command(next_help_heading = "Utility")]
    RollbackToBlock0,

    /// Roll back the chain to a specific block height (DANGER: drops all blocks above target height)
    #[command(next_help_heading = "Utility")]
    RollbackToHeight {
        /// Target height to roll back to (e.g. 274)
        height: u64,
    },

    /// Reset the BFT finality lock to a lower height (DANGER: only use to recover a stuck fork node)
    #[command(next_help_heading = "Utility")]
    ResetFinalityLock {
        /// Target height to reset the finality lock to (must be below current confirmed height)
        height: u64,
    },

    /// Cleanup expired UTXO locks (older than 10 minutes)
    #[command(next_help_heading = "Utility")]
    CleanupLockedUTXOs,

    /// List all currently locked UTXOs
    #[command(next_help_heading = "Utility")]
    ListLockedUTXOs,

    /// Manually unlock a specific UTXO (txid vout)
    #[command(next_help_heading = "Utility")]
    UnlockUTXO {
        /// Transaction ID
        txid: String,
        /// Output index
        vout: u32,
    },

    /// Force-unlock a masternode collateral UTXO stuck in the collateral lock map.
    /// Use when a UTXO shows as "locked" in the dashboard but `listlockedutxos` shows nothing.
    #[command(next_help_heading = "Utility")]
    UnlockCollateral {
        /// Transaction ID of the collateral UTXO
        txid: String,
        /// Output index of the collateral UTXO
        vout: u32,
    },

    /// Scan and unlock orphaned UTXOs (locked by non-existent transactions)
    #[command(next_help_heading = "Utility")]
    UnlockOrphanedUTXOs,

    /// Force unlock ALL UTXOs (nuclear option - use only for recovery)
    #[command(next_help_heading = "Utility")]
    ForceUnlockAll,

    /// Clear stuck finalized transactions and revert their UTXO changes
    #[command(next_help_heading = "Utility")]
    ClearStuckTransactions,
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "lowercase")]
enum MasternodeCommands {
    /// Generate a new masternode private key
    Genkey,
    /// Get masternode information (connected only by default)
    List {
        /// Show all masternodes including disconnected
        #[arg(long)]
        all: bool,
    },
    /// Get masternode status
    Status,
}

#[derive(Serialize, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: String,
    method: String,
    params: Value,
}

#[derive(Serialize, Deserialize, Debug)]
struct RpcResponse {
    jsonrpc: String,
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize, Deserialize, Debug)]
struct RpcError {
    code: i32,
    message: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if let Err(e) = run_command(args).await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

use timed::rpc::credentials::{read_conf_credentials, read_conf_rpctls, read_cookie_file};

async fn run_command(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    // Build HTTP client: always accept self-signed certs (P2P nodes use self-signed RPC certs).
    let detect_client = HttpClient::new()
        .with_timeout(std::time::Duration::from_secs(2))
        .with_accept_invalid_certs(true);

    let (rpc_url, is_testnet) = if let Some(url) = &args.rpc_url {
        let testnet = args.testnet || url.contains("24101");
        (url.clone(), testnet)
    } else {
        // Auto-detect: prefer HTTPS (server default) then fall back to HTTP
        let mut detected = None;
        for (base_url, testnet) in &[("127.0.0.1:24101", true), ("127.0.0.1:24001", false)] {
            let use_tls = read_conf_rpctls(*testnet);
            let schemes: &[&str] = if use_tls {
                &["https", "http"]
            } else {
                &["http"]
            };
            for scheme in schemes {
                let url = format!("{}://{}", scheme, base_url);
                let (user, pass) = read_cookie_file(*testnet)
                    .or_else(|| read_conf_credentials(*testnet))
                    .unwrap_or_default();
                let probe = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "getblockchaininfo",
                    "params": []
                });
                let auth = if !user.is_empty() && !pass.is_empty() {
                    Some((user.as_str(), pass.as_str()))
                } else {
                    None
                };
                if let Ok(response) = detect_client.post_json(&url, &probe, auth).await {
                    if response.is_success() {
                        if let Ok(rpc_response) = response.json::<serde_json::Value>() {
                            if rpc_response.get("result").is_some() {
                                detected = Some((url, *testnet));
                                break;
                            }
                        }
                    }
                }
            }
            if detected.is_some() {
                break;
            }
        }
        detected.unwrap_or_else(|| {
            let testnet = args.testnet;
            let port = if testnet { 24101 } else { 24001 };
            let use_tls = read_conf_rpctls(testnet);
            let scheme = if use_tls { "https" } else { "http" };
            (format!("{}://127.0.0.1:{}", scheme, port), testnet)
        })
    };

    // Resolve RPC credentials: CLI flags > .cookie file > time.conf
    let (rpc_user, rpc_pass) = match (&args.rpcuser, &args.rpcpassword) {
        (Some(u), Some(p)) => (u.clone(), p.clone()),
        _ => read_cookie_file(is_testnet)
            .or_else(|| read_conf_credentials(is_testnet))
            .unwrap_or_default(),
    };

    let client = HttpClient::new()
        .with_timeout(std::time::Duration::from_secs(30))
        .with_accept_invalid_certs(true);

    // ── DumpPrivKey: offline wallet key export (no RPC) ──────────────────────
    if let Commands::DumpPrivKey {
        wallet_path,
        wallet_password,
    } = &args.command
    {
        use timed::wallet::Wallet;

        let wpath = if let Some(p) = wallet_path {
            std::path::PathBuf::from(p)
        } else {
            let data_dir = if is_testnet {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".timecoin")
                    .join("testnet")
            } else {
                dirs::home_dir().unwrap_or_default().join(".timecoin")
            };
            data_dir.join("wallet.dat")
        };

        let wallet = Wallet::load(&wpath, wallet_password)
            .map_err(|e| format!("Failed to load wallet from {}: {}", wpath.display(), e))?;

        let privkey_hex = hex::encode(wallet.signing_key().to_bytes());
        let pubkey_hex = hex::encode(wallet.public_key().as_bytes());
        println!("address:    {}", wallet.address());
        println!("pubkey:     {}", pubkey_hex);
        println!("privkey:    {}", privkey_hex);
        return Ok(());
    }
    // ── End DumpPrivKey ───────────────────────────────────────────────────────

    // ── MasternodeReg: local signing then sendrawtransaction ─────────────────
    if let Commands::MasternodeReg {
        collateral,
        masternode_ip,
        port,
        payout_address,
        wallet_path,
        wallet_password,
        privkey,
    } = &args.command
    {
        use ed25519_dalek::{Signer, SigningKey};

        use timed::wallet::Wallet;

        let p2p_port = port.unwrap_or(if is_testnet { 24100 } else { 24000 });

        // Parse collateral outpoint "txid_hex:vout"
        let parts: Vec<&str> = collateral.rsplitn(2, ':').collect();
        if parts.len() != 2 {
            return Err("--collateral must be in txid_hex:vout format".into());
        }
        let vout: u32 = parts[0]
            .parse()
            .map_err(|_| "Invalid vout in --collateral")?;
        let txid_hex = parts[1];
        let outpoint_str = format!("{}:{}", txid_hex, vout);

        // Build signing key
        let owned_key: SigningKey;
        let signing_key: &SigningKey = if let Some(pk_hex) = privkey {
            let bytes = hex::decode(pk_hex)
                .map_err(|_| "--privkey must be 64 hex characters (32 bytes)")?;
            if bytes.len() != 32 {
                return Err("--privkey must be exactly 32 bytes (64 hex chars)".into());
            }
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            owned_key = SigningKey::from_bytes(&arr);
            &owned_key
        } else {
            let wpath = if let Some(p) = wallet_path {
                std::path::PathBuf::from(p)
            } else {
                let data_dir = if is_testnet {
                    dirs::home_dir()
                        .unwrap_or_default()
                        .join(".timecoin")
                        .join("testnet")
                } else {
                    dirs::home_dir().unwrap_or_default().join(".timecoin")
                };
                data_dir.join("wallet.dat")
            };
            let wallet = Wallet::load(&wpath, wallet_password).map_err(|e| {
                format!(
                    "Failed to load wallet from {}: {}. Use --wallet-path or --privkey.",
                    wpath.display(),
                    e
                )
            })?;
            owned_key = wallet.signing_key().clone();
            &owned_key
        };

        let node_address = format!("{}:{}", masternode_ip, p2p_port);
        let pubkey_hex = hex::encode(signing_key.verifying_key().as_bytes());
        let msg = format!(
            "MNREG:{}:{}:{}:{}",
            node_address, payout_address, pubkey_hex, outpoint_str
        );
        let signature_hex = hex::encode(signing_key.sign(msg.as_bytes()).to_bytes());

        // Build the transaction
        let tx = timed::types::Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
            special_data: Some(
                timed::types::SpecialTransactionData::MasternodeRegistration {
                    node_address: node_address.clone(),
                    wallet_address: payout_address.clone(),
                    reward_address: String::new(),
                    collateral_outpoint: outpoint_str.clone(),
                    pubkey: pubkey_hex.clone(),
                    signature: signature_hex,
                },
            ),
            encrypted_memo: None,
        };

        let tx_hex = hex::encode(bincode::serialize(&tx)?);

        // Submit via sendrawtransaction RPC
        let auth = if !rpc_user.is_empty() && !rpc_pass.is_empty() {
            Some((rpc_user.as_str(), rpc_pass.as_str()))
        } else {
            None
        };
        let request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: "masternodereg".to_string(),
            method: "sendrawtransaction".to_string(),
            params: json!([tx_hex]),
        };
        let response = client.post_json(&rpc_url, &request, auth).await?;
        let rpc_response: RpcResponse = response.json()?;
        if let Some(error) = rpc_response.error {
            return Err(format!(
                "MasternodeReg rejected: {} (code {})\n\
                 Check: collateral UTXO exists, owner_pubkey matches utxo.address, \
                 and collateral is not already on-chain registered.",
                error.message, error.code
            )
            .into());
        }
        if let Some(txid) = rpc_response.result {
            println!("✅ MasternodeRegistration submitted!");
            println!("   txid:            {}", txid);
            println!("   collateral:      {}", outpoint_str);
            println!("   masternode:      {}:{}", masternode_ip, p2p_port);
            println!("   payout_address:  {}", payout_address);
            println!("   pubkey:          {}", pubkey_hex);
            println!("\nThe masternode will be recognized as the registered operator once");
            println!("the transaction is confirmed in the next block.");
        }
        return Ok(());
    }
    // ── End MasternodeReg ────────────────────────────────────────────────────

    // ── AggregateBlacklists: query multiple nodes and merge ──────────────────
    if let Commands::AggregateBlacklists { nodes } = &args.command {
        let mut query_urls: Vec<String> = nodes.clone();
        // Always include localhost
        if query_urls.is_empty() || !query_urls.iter().any(|u| u.contains("127.0.0.1") || u.contains("localhost")) {
            let port = if is_testnet { 24101 } else { 24001 };
            let use_tls = timed::rpc::credentials::read_conf_rpctls(is_testnet);
            let scheme = if use_tls { "https" } else { "http" };
            query_urls.insert(0, format!("{}://127.0.0.1:{}", scheme, port));
        }

        let auth = if !rpc_user.is_empty() && !rpc_pass.is_empty() {
            Some((rpc_user.as_str(), rpc_pass.as_str()))
        } else {
            None
        };
        let blacklist_req = serde_json::json!({
            "jsonrpc": "2.0", "id": "agg", "method": "getblacklist", "params": []
        });

        // ip -> (ban_reason, Vec<node_url>)
        let mut permanent_map: std::collections::HashMap<String, (String, Vec<String>)> = std::collections::HashMap::new();
        let mut node_results: Vec<(String, u64, u64)> = Vec::new(); // (url, perm_count, temp_count)
        let mut all_violations: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

        for url in &query_urls {
            match client.post_json(url, &blacklist_req, auth).await {
                Ok(resp) if resp.is_success() => {
                    if let Ok(rpc_resp) = resp.json::<RpcResponse>() {
                        if let Some(result) = rpc_resp.result {
                            let perm_count = result.get("summary")
                                .and_then(|s| s.get("permanent_bans"))
                                .and_then(|v| v.as_u64()).unwrap_or(0);
                            let temp_count = result.get("summary")
                                .and_then(|s| s.get("temporary_bans"))
                                .and_then(|v| v.as_u64()).unwrap_or(0);
                            node_results.push((url.clone(), perm_count, temp_count));

                            if let Some(perms) = result.get("permanent").and_then(|v| v.as_array()) {
                                for entry in perms {
                                    let ip = entry.get("ip").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let reason = entry.get("reason").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let e = permanent_map.entry(ip).or_insert_with(|| (reason.clone(), Vec::new()));
                                    e.1.push(url.clone());
                                }
                            }
                            if let Some(viols) = result.get("violations").and_then(|v| v.as_array()) {
                                for entry in viols {
                                    let ip = entry.get("ip").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    let count = entry.get("violations").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let e = all_violations.entry(ip).or_insert(0);
                                    *e += count;
                                }
                            }
                        }
                    }
                }
                _ => {
                    eprintln!("⚠️  Could not reach {}", url);
                }
            }
        }

        println!("=== Aggregated Blacklist ({} nodes queried) ===\n", query_urls.len());
        println!("Node Summary:");
        for (url, perm, temp) in &node_results {
            println!("  {:45}  {} permanent  {} temporary", url, perm, temp);
        }

        // Sort by node count (most-banned first)
        let mut sorted: Vec<_> = permanent_map.iter().collect();
        sorted.sort_by(|a, b| b.1.1.len().cmp(&a.1.1.len()).then(a.0.cmp(b.0)));

        println!("\n--- Permanently Banned IPs ({} unique) ---", sorted.len());
        println!("{:<22} {:>6}  {}", "IP", "Nodes", "Reason (first seen)");
        println!("{}", "-".repeat(90));
        for (ip, (reason, node_urls)) in &sorted {
            let short_reason = if reason.len() > 55 { &reason[..55] } else { reason };
            println!("{:<22} {:>6}  {}", ip, node_urls.len(), short_reason);
        }

        if !all_violations.is_empty() {
            let mut viols: Vec<_> = all_violations.iter().collect();
            viols.sort_by(|a, b| b.1.cmp(a.1));
            println!("\n--- Top Violation Counts (aggregate) ---");
            for (ip, count) in viols.iter().take(20) {
                println!("  {:<22}  {} violation(s)", ip, count);
            }
        }

        // IPs banned on ALL queried nodes
        let all_node_count = query_urls.len();
        let banned_everywhere: Vec<_> = sorted.iter()
            .filter(|(_, (_, nodes))| nodes.len() == all_node_count)
            .collect();
        if !banned_everywhere.is_empty() {
            println!("\n--- Banned on ALL {} nodes ---", all_node_count);
            for (ip, _) in &banned_everywhere {
                println!("  {}", ip);
            }
        }

        return Ok(());
    }
    // ── End AggregateBlacklists ──────────────────────────────────────────────

    let (method, params) = match &args.command {
        Commands::GetBlockchainInfo => ("getblockchaininfo", json!([])),
        Commands::GetBlock { height } => ("getblock", json!([height])),
        Commands::GetBlockCount => ("getblockcount", json!([])),
        Commands::GetBestBlockHash => ("getbestblockhash", json!([])),
        Commands::GetBlockHash { height } => ("getblockhash", json!([height])),
        Commands::GetBlockHeader { height_or_hash } => ("getblockheader", json!([height_or_hash])),
        Commands::GetNetworkInfo => ("getnetworkinfo", json!([])),
        Commands::GetPeerInfo => ("getpeerinfo", json!([])),
        Commands::GetConnectionCount => ("getconnectioncount", json!([])),
        Commands::GetWhitelist => ("getwhitelist", json!([])),
        Commands::RemoveWhitelist { ip } => ("removewhitelist", json!([ip])),
        Commands::AuditCollateral => ("auditcollateral", json!([])),
        Commands::GetBlacklist => ("getblacklist", json!([])),
        Commands::Ban { ip, reason } => (
            "ban",
            json!([ip, reason.as_deref().unwrap_or("manual ban via CLI")]),
        ),
        Commands::Unban { ip } => ("unban", json!([ip])),
        Commands::ClearBanList => ("clearbanlist", json!([])),
        Commands::AddWhitelist { ip } => ("addwhitelist", json!([ip])),
        Commands::GetTxOutSetInfo => ("gettxoutsetinfo", json!([])),
        Commands::GetTxOut { txid, vout } => ("gettxout", json!([txid, vout])),
        Commands::GetTransaction { txid } => ("gettransaction", json!([txid])),
        Commands::GetRawTransaction { txid, verbose } => {
            ("getrawtransaction", json!([txid, verbose]))
        }
        Commands::SendRawTransaction { hex } => ("sendrawtransaction", json!([hex])),
        Commands::GetTransactionFinality { txid } => ("gettransactionfinality", json!([txid])),
        Commands::EstimateSmartFee { conf_target } => ("estimatesmartfee", json!([conf_target])),
        Commands::CreateRawTransaction { inputs, outputs } => {
            let inputs_json: Value = serde_json::from_str(inputs)?;
            let outputs_json: Value = serde_json::from_str(outputs)?;
            ("createrawtransaction", json!([inputs_json, outputs_json]))
        }
        Commands::DecodeRawTransaction { hex } => ("decoderawtransaction", json!([hex])),
        Commands::GetBalance { address } => {
            if let Some(addr) = address {
                ("getbalance", json!([addr]))
            } else {
                ("getbalance", json!([]))
            }
        }
        Commands::ListUnspent {
            limit,
            minconf,
            maxconf,
        } => ("listunspent", json!([minconf, maxconf, null, limit])),
        Commands::GetNewAddress => ("getnewaddress", json!([])),
        Commands::GetWalletInfo => ("getwalletinfo", json!([])),
        Commands::ListTransactions { count } => ("listtransactions", json!([count])),
        Commands::GetAddressInfo { address } => ("getaddressinfo", json!([address])),
        Commands::ListUnspentMulti { addresses } => {
            let addrs: Value = serde_json::from_str(addresses)
                .map_err(|_| "addresses must be a JSON array, e.g. '[\"addr1\",\"addr2\"]'")?;
            ("listunspentmulti", json!([addrs]))
        }
        Commands::SignMessage { message, address } => ("signmessage", json!([address, message])),
        Commands::VerifyMessage { address, signature, message } => {
            ("verifymessage", json!([address, signature, message]))
        }
        Commands::ListReceivedByAddress {
            minconf,
            include_empty,
        } => ("listreceivedbyaddress", json!([minconf, include_empty])),
        Commands::MasternodeList { all } => ("masternodelist", json!([all])),
        Commands::MasternodeStatus => ("masternodestatus", json!([])),
        Commands::Masternode { subcmd } => match subcmd {
            MasternodeCommands::Genkey => ("masternodegenkey", json!([])),
            MasternodeCommands::List { all } => ("masternodelist", json!([all])),
            MasternodeCommands::Status => ("masternodestatus", json!([])),
        },
        Commands::ReleaseAllCollaterals => ("releaseallcollaterals", json!([])),
        Commands::ListLockedCollaterals => ("listlockedcollaterals", json!([])),
        Commands::MasternodeRegStatus { address } => ("masternoderegstatus", json!([address])),
        Commands::CheckCollateral => ("checkcollateral", json!([])),
        Commands::FindCollateral { outpoint } => ("findcollateral", json!([outpoint])),
        Commands::MasternodeReg { .. } => unreachable!("handled above"),
        Commands::DumpPrivKey { .. } => unreachable!("handled above"),
        Commands::AggregateBlacklists { .. } => unreachable!("handled above"),
        Commands::GetConsensusInfo => ("getconsensusinfo", json!([])),
        Commands::GetTimeVoteStatus => ("gettimevotestatus", json!([])),
        Commands::GetTreasuryBalance => ("gettreasurybalance", json!([])),
        Commands::ListProposals => ("listproposals", json!([])),
        Commands::GetProposal { proposal_id } => ("getproposal", json!([proposal_id])),
        Commands::SubmitProposal { proposal_type, data } => {
            let data_val: Value = serde_json::from_str(data)
                .map_err(|_| "data must be valid JSON")?;
            ("submitproposal", json!([proposal_type, data_val]))
        }
        Commands::VoteProposal { proposal_id, vote } => ("voteproposal", json!([proposal_id, vote])),
        Commands::ValidateAddress { address } => ("validateaddress", json!([address])),
        Commands::Stop => ("stop", json!([])),
        Commands::Uptime => ("uptime", json!([])),
        Commands::GetInfo => ("getinfo", json!([])),
        Commands::GetFeeSchedule => ("getfeeschedule", json!([])),
        Commands::GetTxIndexStatus => ("gettxindexstatus", json!([])),
        Commands::ReindexTransactions => ("reindextransactions", json!([])),
        Commands::Reindex => ("reindex", json!([])),
        Commands::RollbackToBlock0 => ("rollbacktoblock0", json!([])),
        Commands::RollbackToHeight { height } => ("rollbacktoheight", json!([height])),
        Commands::ResetFinalityLock { height } => ("resetfinalitylock", json!([height])),
        Commands::CleanupLockedUTXOs => ("cleanuplockedutxos", json!([])),
        Commands::ListLockedUTXOs => ("listlockedutxos", json!([])),
        Commands::UnlockUTXO { txid, vout } => ("unlockutxo", json!([txid, vout])),
        Commands::UnlockCollateral { txid, vout } => ("unlockcollateral", json!([txid, vout])),
        Commands::UnlockOrphanedUTXOs => ("unlockorphanedutxos", json!([])),
        Commands::ForceUnlockAll => ("forceunlockall", json!([])),
        Commands::ClearStuckTransactions => ("clearstucktransactions", json!([])),
        Commands::GetMempoolInfo => ("getmempoolinfo", json!([])),
        Commands::GetRawMempool { verbose } => ("getrawmempool", json!([verbose])),
        Commands::GetMempoolVerbose => ("getmempoolverbose", json!([])),
        Commands::SendToAddress {
            address,
            amount,
            subtract_fee,
            nowait,
            memo,
        } => (
            "sendtoaddress",
            json!([address, amount, subtract_fee, nowait, memo]),
        ),
        Commands::SendFrom {
            from_address,
            to_address,
            amount,
            subtract_fee,
            nowait,
        } => (
            "sendfrom",
            json!([from_address, to_address, amount, subtract_fee, nowait]),
        ),
        Commands::MergeUtxos {
            min_count,
            max_count,
            address,
        } => ("mergeutxos", json!([min_count, max_count, address])),
        Commands::RequestPayment {
            amount,
            memo,
            label,
        } => ("createpaymentrequest", json!([amount, memo, label])),
        Commands::PayRequest { uri, memo } => ("paypaymentrequest", json!([uri, memo])),
        Commands::SendPaymentRequest {
            from_address,
            to_address,
            amount,
            memo,
            pubkey,
            signature,
            timestamp,
            requester_name,
        } => (
            "sendpaymentrequest",
            json!([
                from_address,
                to_address,
                amount,
                memo,
                pubkey,
                signature,
                timestamp,
                requester_name
            ]),
        ),
        Commands::GetPaymentRequests { address } => ("getpaymentrequests", json!([address])),
        Commands::RespondPaymentRequest {
            request_id,
            requester_address,
            payer_address,
            accepted,
            txid,
        } => (
            "respondpaymentrequest",
            json!([request_id, requester_address, payer_address, accepted, txid]),
        ),
        Commands::CancelPaymentRequest {
            request_id,
            requester_address,
        } => (
            "cancelpaymentrequest",
            json!([request_id, requester_address]),
        ),
        Commands::MarkPaymentRequestViewed {
            request_id,
            requester_address,
            payer_address,
        } => (
            "markpaymentrequestviewed",
            json!([request_id, requester_address, payer_address]),
        ),
    };

    let request = RpcRequest {
        jsonrpc: "2.0".to_string(),
        id: "time-cli".to_string(),
        method: method.to_string(),
        params,
    };

    // Send request; if HTTPS fails, retry over HTTP with a warning (allows use when cert issues)
    let auth = if !rpc_user.is_empty() && !rpc_pass.is_empty() {
        Some((rpc_user.as_str(), rpc_pass.as_str()))
    } else {
        None
    };
    let response = match client.post_json(&rpc_url, &request, auth).await {
        Ok(r) => r,
        Err(e) if rpc_url.starts_with("https://") => {
            let http_url = rpc_url.replacen("https://", "http://", 1);
            eprintln!(
                "⚠️  HTTPS RPC failed ({}); retrying over plain HTTP — check rpctlscert/rpctlskey",
                e
            );
            client.post_json(&http_url, &request, auth).await?
        }
        Err(e) => return Err(e.into()),
    };

    if response.status == 401 {
        return Err("RPC authentication failed. Check rpcuser/rpcpassword in time.conf or use --rpcuser/--rpcpassword flags.".into());
    }

    if !response.is_success() {
        return Err(format!("HTTP error: {}", response.status).into());
    }

    let rpc_response: RpcResponse = response
        .json()
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if let Some(error) = rpc_response.error {
        return Err(format!("RPC error {}: {}", error.code, error.message).into());
    }

    if let Some(result) = rpc_response.result {
        if args.human {
            print_human_readable(&args.command, &result)?;
        } else if args.compact {
            println!("{}", serde_json::to_string(&result)?);
        } else {
            // Default: pretty JSON (like Bitcoin).
            // Bare string results print without quotes, matching Bitcoin CLI behaviour.
            if let Some(s) = result.as_str() {
                println!("{}", s);
            } else {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
        }
    }

    Ok(())
}

fn print_human_readable(
    command: &Commands,
    result: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Commands::GetBlockchainInfo => {
            println!("Blockchain Information:");
            println!(
                "  Chain:            {}",
                result
                    .get("chain")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Blocks:           {}",
                result.get("blocks").and_then(|v| v.as_u64()).unwrap_or(0)
            );
            println!(
                "  Consensus:        {}",
                result
                    .get("consensus")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Instant Finality: {}",
                result
                    .get("instant_finality")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            );
        }
        Commands::GetBlockCount => {
            println!("Block Height: {}", result.as_u64().unwrap_or(0));
        }
        Commands::GetBestBlockHash => {
            println!("Best Block Hash: {}", result.as_str().unwrap_or("N/A"));
        }
        Commands::GetBlockHash { .. } => {
            println!("Block Hash: {}", result.as_str().unwrap_or("N/A"));
        }
        Commands::GetBalance { .. } => {
            if let Some(obj) = result.as_object() {
                let balance = obj.get("balance").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let locked = obj.get("locked").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let available = obj.get("available").and_then(|v| v.as_f64()).unwrap_or(0.0);

                println!("Wallet Balance:");
                println!("  Total:         {:.8} TIME", balance);
                println!("  Locked:        {:.8} TIME (collateral)", locked);
                println!("  Available:     {:.8} TIME (spendable)", available);
            } else {
                // Fallback for old format
                println!("Balance: {} TIME", result.as_f64().unwrap_or(0.0));
            }
        }
        Commands::GetNewAddress => {
            println!("Address: {}", result.as_str().unwrap_or("N/A"));
        }
        Commands::ListReceivedByAddress { .. } => {
            if let Some(addresses) = result.as_array() {
                println!("\nAddresses with Received Funds:");
                println!("{:<50} {:>15} {:>10}", "Address", "Amount (TIME)", "TXs");
                println!("{}", "-".repeat(77));

                for addr_info in addresses {
                    let address = addr_info
                        .get("address")
                        .and_then(|v| v.as_str())
                        .unwrap_or("N/A");
                    let amount = addr_info
                        .get("amount")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let txcount = addr_info
                        .get("txcount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    println!("{:<50} {:>15.8} {:>10}", address, amount, txcount);
                }

                println!("\nTotal Addresses: {}", addresses.len());
            } else {
                println!("No addresses found");
            }
        }
        Commands::GetWalletInfo => {
            println!("Wallet Information:");
            println!(
                "  Wallet Name:          {}",
                result
                    .get("walletname")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Balance:              {} TIME",
                result
                    .get("balance")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            );
            println!(
                "  Locked:               {} TIME (collateral)",
                result.get("locked").and_then(|v| v.as_f64()).unwrap_or(0.0)
            );
            println!(
                "  Available:            {} TIME (spendable)",
                result
                    .get("available")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            );
            println!(
                "  Unconfirmed Balance:  {} TIME",
                result
                    .get("unconfirmed_balance")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            );
            println!(
                "  Immature Balance:     {} TIME",
                result
                    .get("immature_balance")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            );
            println!(
                "  Transaction Count:    {}",
                result.get("txcount").and_then(|v| v.as_u64()).unwrap_or(0)
            );
            println!(
                "  Keypool Size:         {}",
                result
                    .get("keypoolsize")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
            println!(
                "  Pay TX Fee:           {}",
                result
                    .get("paytxfee")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            );
        }
        Commands::ListTransactions { .. } => {
            if let Some(txs) = result.as_array() {
                println!("\nRecent Wallet Transactions:");
                println!(
                    "{:<10} {:>15} {:>8} {:>8} {:<64}",
                    "Category", "Amount (TIME)", "Confs", "Height", "TxID"
                );
                println!("{}", "-".repeat(110));
                for tx in txs {
                    let category = tx.get("category").and_then(|v| v.as_str()).unwrap_or("?");
                    let amount = tx.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let confs = tx
                        .get("confirmations")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let height = tx.get("blockheight").and_then(|v| v.as_u64()).unwrap_or(0);
                    let txid = tx.get("txid").and_then(|v| v.as_str()).unwrap_or("");
                    let fee_str = tx
                        .get("fee")
                        .and_then(|v| v.as_f64())
                        .map(|f| format!(" (fee: {:.8})", f))
                        .unwrap_or_default();

                    println!(
                        "{:<10} {:>15.8} {:>8} {:>8} {:<64}{}",
                        category, amount, confs, height, txid, fee_str
                    );
                }
                println!("\nTotal: {} transaction(s)", txs.len());
            } else {
                println!("No transactions found");
            }
        }
        Commands::ListUnspent { limit, .. } => {
            if let Some(utxos) = result.as_array() {
                println!("Unspent Transaction Outputs (sorted by amount, descending):");
                println!(
                    "{:<66} {:>4} {:<42} {:>12}",
                    "TxID", "Vout", "Address", "Amount"
                );
                println!("{}", "-".repeat(130));
                for utxo in utxos {
                    let txid = utxo.get("txid").and_then(|v| v.as_str()).unwrap_or("");
                    let vout = utxo.get("vout").and_then(|v| v.as_u64()).unwrap_or(0);
                    let address = utxo.get("address").and_then(|v| v.as_str()).unwrap_or("");
                    let amount = utxo.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    println!("{:<66} {:>4} {:<42} {:>12.8}", txid, vout, address, amount);
                }
                let total_count_msg = if *limit == 0 {
                    format!("Total UTXOs: {} (all)", utxos.len())
                } else {
                    format!("Showing: {} (use -n 0 to show all)", utxos.len())
                };
                println!("\n{}", total_count_msg);
            }
        }
        Commands::MasternodeList { all: _ } => {
            if let Some(obj) = result.as_object() {
                if let Some(nodes) = obj.get("masternodes").and_then(|v| v.as_array()) {
                    let show_all = obj
                        .get("show_all")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let total_in_registry = obj
                        .get("total_in_registry")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    if show_all {
                        println!("All Masternodes:");
                    } else {
                        println!("Connected Masternodes:");
                    }
                    println!(
                        "{:<42} {:<10} {:<8} {:<11} {:<12} {:<12}",
                        "Address", "Tier", "Active", "Connected", "Uptime", "Collateral"
                    );
                    println!("{}", "-".repeat(103));

                    let mut connected_count = 0;
                    for node in nodes {
                        let address = node.get("address").and_then(|v| v.as_str()).unwrap_or("");
                        let tier = node.get("tier").and_then(|v| v.as_str()).unwrap_or("");
                        let active = node
                            .get("is_active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let connected = node
                            .get("is_connected")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if connected {
                            connected_count += 1;
                        }
                        let uptime = node
                            .get("total_uptime")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let collateral_locked = node
                            .get("collateral_locked")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        let collateral_status = if collateral_locked {
                            "🔒 Locked"
                        } else {
                            "Legacy"
                        };

                        println!(
                            "{:<42} {:<10} {:<8} {:<11} {:<12} {:<12}",
                            address, tier, active, connected, uptime, collateral_status
                        );
                    }

                    if show_all {
                        println!(
                            "\nShowing: {} masternodes ({} connected, {} disconnected)",
                            nodes.len(),
                            connected_count,
                            nodes.len() - connected_count
                        );
                    } else {
                        println!(
                            "\nShowing: {} connected masternodes (Total in registry: {})",
                            nodes.len(),
                            total_in_registry
                        );
                        println!(
                            "💡 Use --all flag to show all masternodes including disconnected"
                        );
                    }
                }
            }
        }
        Commands::ListLockedCollaterals => {
            if let Some(obj) = result.as_object() {
                if let Some(collaterals) = obj.get("collaterals").and_then(|v| v.as_array()) {
                    println!("Locked Collaterals:");
                    println!(
                        "{:<68} {:<42} {:>16} {:>12}",
                        "Outpoint", "Masternode", "Amount (TIME)", "Height"
                    );
                    println!("{}", "-".repeat(145));
                    for col in collaterals {
                        let outpoint = col.get("outpoint").and_then(|v| v.as_str()).unwrap_or("");
                        let mn_addr = col
                            .get("masternode_address")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let amount = col
                            .get("amount_time")
                            .and_then(|v| v.as_str())
                            .unwrap_or("0");
                        let height = col.get("lock_height").and_then(|v| v.as_u64()).unwrap_or(0);

                        println!(
                            "{:<68} {:<42} {:>16} {:>12}",
                            outpoint, mn_addr, amount, height
                        );
                    }
                    println!("\nTotal Locked: {}", collaterals.len());
                }
            }
        }
        Commands::GetPeerInfo => {
            if let Some(peers) = result.as_array() {
                println!("Connected Peers:");
                println!("{:<45} {:<10} {:<10}", "Address", "Version", "Subversion");
                println!("{}", "-".repeat(70));
                for peer in peers {
                    let addr = peer.get("addr").and_then(|v| v.as_str()).unwrap_or("");
                    let version = peer.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
                    let subver = peer.get("subver").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{:<45} {:<10} {:<10}", addr, version, subver);
                }
                println!("\nTotal Peers: {}", peers.len());
            }
        }
        Commands::AuditCollateral => {
            let evicted = result
                .get("squatters_evicted")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let warnings = result.get("warnings").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("=== Collateral Audit Results ===");
            println!("Squatters evicted:  {}", evicted);
            println!("Unresolved (stalemate, V4 required): {}", warnings);
            if let Some(list) = result.get("evicted").and_then(|v| v.as_array()) {
                if !list.is_empty() {
                    println!("\n--- Evicted Squatters ---");
                    for entry in list {
                        let ip = entry.get("ip").and_then(|v| v.as_str()).unwrap_or("");
                        let outpoint = entry.get("outpoint").and_then(|v| v.as_str()).unwrap_or("");
                        let reason = entry.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                        println!("  {} — {} — {}", ip, outpoint, reason);
                    }
                }
            }
            if let Some(list) = result.get("unresolved_warnings").and_then(|v| v.as_array()) {
                if !list.is_empty() {
                    println!("\n--- Unresolved Stalemates ---");
                    for entry in list {
                        println!("  {}", entry);
                    }
                }
            }
        }
        Commands::GetBlacklist => {
            let summary = result.get("summary");
            if let Some(s) = summary {
                let perm = s
                    .get("permanent_bans")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let temp = s
                    .get("temporary_bans")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let viol = s
                    .get("active_violations")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let wl = s.get("whitelisted").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("=== Blacklist Summary ===");
                println!("Permanent bans:   {}", perm);
                println!("Temporary bans:   {}", temp);
                println!("Active violators: {}", viol);
                println!("Whitelisted:      {}", wl);
            }
            if let Some(perms) = result.get("permanent").and_then(|v| v.as_array()) {
                if !perms.is_empty() {
                    println!("\n--- Permanent Bans ---");
                    for entry in perms {
                        let ip = entry.get("ip").and_then(|v| v.as_str()).unwrap_or("");
                        let reason = entry.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                        println!("  {:<20}  {}", ip, reason);
                    }
                }
            }
            if let Some(temps) = result.get("temporary").and_then(|v| v.as_array()) {
                if !temps.is_empty() {
                    println!("\n--- Temporary Bans ---");
                    for entry in temps {
                        let ip = entry.get("ip").and_then(|v| v.as_str()).unwrap_or("");
                        let secs = entry
                            .get("remaining_secs")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let reason = entry.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                        println!("  {:<20}  {}s remaining  {}", ip, secs, reason);
                    }
                }
            }
            if let Some(subnets) = result.get("subnets").and_then(|v| v.as_array()) {
                if !subnets.is_empty() {
                    println!("\n--- Subnet Bans ---");
                    for entry in subnets {
                        let cidr = entry.get("subnet").and_then(|v| v.as_str()).unwrap_or("");
                        let reason = entry.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                        println!("  {:<20}  {}", cidr, reason);
                    }
                }
            }
            if let Some(viols) = result.get("violations").and_then(|v| v.as_array()) {
                if !viols.is_empty() {
                    println!("\n--- Top Violation Counts ---");
                    for entry in viols.iter().take(20) {
                        let ip = entry.get("ip").and_then(|v| v.as_str()).unwrap_or("");
                        let count = entry
                            .get("violations")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        println!("  {:<20}  {} violation(s)", ip, count);
                    }
                }
            }
        }
        Commands::Ban { .. }
        | Commands::Unban { .. }
        | Commands::ClearBanList
        | Commands::AddWhitelist { .. } => {
            let msg = result
                .get("message")
                .and_then(|v| v.as_str())
                .or_else(|| result.get("result").and_then(|v| v.as_str()))
                .unwrap_or("Done");
            println!("{}", msg);
        }
        Commands::Uptime => {
            let seconds = result.as_u64().unwrap_or(0);
            let days = seconds / 86400;
            let hours = (seconds % 86400) / 3600;
            let minutes = (seconds % 3600) / 60;
            let secs = seconds % 60;
            println!(
                "Uptime: {} days, {} hours, {} minutes, {} seconds",
                days, hours, minutes, secs
            );
        }
        Commands::GetInfo => {
            // Display general node information
            println!("=== Node Information ===");
            if let Some(version) = result.get("version").and_then(|v| v.as_str()) {
                println!("Version:         {}", version);
            }
            if let Some(blocks) = result.get("blocks").and_then(|v| v.as_u64()) {
                println!("Blocks:          {}", blocks);
            }
            if let Some(connections) = result.get("connections").and_then(|v| v.as_u64()) {
                println!("Connections:     {}", connections);
            }
            if let Some(masternodes) = result.get("masternodes").and_then(|v| v.as_u64()) {
                println!("Masternodes:     {}", masternodes);
            }
            if let Some(balance) = result.get("balance").and_then(|v| v.as_f64()) {
                println!("Balance:         {} TIME", balance);
            }
            if let Some(uptime) = result.get("uptime").and_then(|v| v.as_u64()) {
                let days = uptime / 86400;
                let hours = (uptime % 86400) / 3600;
                let minutes = (uptime % 3600) / 60;
                println!("Uptime:          {}d {}h {}m", days, hours, minutes);
            }
        }
        Commands::CleanupLockedUTXOs => {
            let cleaned = result.get("cleaned").and_then(|v| v.as_u64()).unwrap_or(0);
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");
            println!("{}", message);
            if cleaned > 0 {
                println!("✓ Successfully cleaned {} expired lock(s)", cleaned);
            } else {
                println!("ℹ No expired locks found");
            }
        }
        Commands::ListLockedUTXOs => {
            let count = result
                .get("locked_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            if count == 0 {
                println!("No locked UTXOs found");
            } else {
                println!("Found {} locked UTXO(s):\n", count);

                if let Some(locked) = result.get("locked_utxos").and_then(|v| v.as_array()) {
                    for utxo in locked {
                        let txid = utxo.get("txid").and_then(|v| v.as_str()).unwrap_or("N/A");
                        let vout = utxo.get("vout").and_then(|v| v.as_u64()).unwrap_or(0);
                        let amount = utxo.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let locked_by = utxo
                            .get("locked_by_tx")
                            .and_then(|v| v.as_str())
                            .unwrap_or("N/A");
                        let age = utxo
                            .get("age_seconds")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        let expired = utxo
                            .get("expired")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        println!("  UTXO: {}:{}", txid, vout);
                        println!("    Amount:     {} TIME", amount);
                        println!("    Locked by:  {}", locked_by);
                        println!("    Age:        {} seconds", age);
                        println!(
                            "    Status:     {}",
                            if expired { "⚠️  EXPIRED" } else { "Active" }
                        );
                        println!();
                    }
                }
            }
        }
        Commands::UnlockUTXO { .. } => {
            let unlocked = result
                .get("unlocked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");

            if unlocked {
                println!("✓ {}", message);
                if let Some(was_locked_by) = result.get("was_locked_by").and_then(|v| v.as_str()) {
                    println!("  Was locked by transaction: {}", was_locked_by);
                }
            } else {
                println!("❌ Failed to unlock: {}", message);
            }
        }
        Commands::UnlockCollateral { .. } => {
            let unlocked = result
                .get("unlocked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");

            if unlocked {
                println!("✓ {}", message);
            } else {
                println!("❌ Failed to unlock collateral: {}", message);
            }
        }
        Commands::ResetFinalityLock { .. } => {
            let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");
            let prev = result
                .get("previous_confirmed_height")
                .and_then(|v| v.as_u64());
            let new = result.get("new_confirmed_height").and_then(|v| v.as_u64());

            if status == "ok" {
                if let (Some(p), Some(n)) = (prev, new) {
                    println!("✓ Finality lock reset: {} → {}", p, n);
                } else {
                    println!("✓ {}", message);
                }
                println!("  {}", message);
            } else {
                println!("❌ Failed to reset finality lock: {}", message);
            }
        }
        Commands::UnlockOrphanedUTXOs => {
            let unlocked = result.get("unlocked").and_then(|v| v.as_u64()).unwrap_or(0);
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");

            println!("{}", message);

            if unlocked > 0 {
                println!("✓ Unlocked {} orphaned UTXO(s)", unlocked);

                if let Some(orphaned) = result.get("orphaned_utxos").and_then(|v| v.as_array()) {
                    println!("\nDetails:");
                    for utxo in orphaned {
                        let txid = utxo.get("txid").and_then(|v| v.as_str()).unwrap_or("N/A");
                        let vout = utxo.get("vout").and_then(|v| v.as_u64()).unwrap_or(0);
                        let amount = utxo.get("amount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let missing_tx = utxo
                            .get("locked_by_missing_tx")
                            .and_then(|v| v.as_str())
                            .unwrap_or("N/A");

                        println!(
                            "  {}:{} - {} TIME (was locked by missing tx: {})",
                            txid,
                            vout,
                            amount,
                            &missing_tx[..16]
                        );
                    }
                }
            } else {
                println!("ℹ No orphaned locks found");
            }
        }
        Commands::ForceUnlockAll => {
            let unlocked = result.get("unlocked").and_then(|v| v.as_u64()).unwrap_or(0);
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");

            println!("⚠️  {}", message);
            if unlocked > 0 {
                println!("   All {} UTXOs have been reset to Unspent state", unlocked);
            }
        }
        Commands::ClearStuckTransactions => {
            let cleared = result.get("cleared").and_then(|v| v.as_u64()).unwrap_or(0);
            let inputs = result
                .get("inputs_restored")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let outputs = result
                .get("outputs_removed")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let message = result
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Done");

            println!("🧹 {}", message);
            if cleared > 0 {
                println!("   Transactions cleared: {}", cleared);
                println!("   Input UTXOs restored:  {}", inputs);
                println!("   Output UTXOs removed:  {}", outputs);
                if let Some(txs) = result.get("transactions").and_then(|v| v.as_array()) {
                    println!("   Transaction IDs:");
                    for tx in txs {
                        if let Some(txid) = tx.as_str() {
                            println!("     • {}…", &txid[..16.min(txid.len())]);
                        }
                    }
                }
            }
        }
        Commands::MasternodeStatus => {
            println!("Masternode Status:");
            println!(
                "  Status:         {}",
                result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            if let Some(addr) = result.get("address").and_then(|v| v.as_str()) {
                println!("  Address:        {}", addr);
                println!(
                    "  Reward Address: {}",
                    result
                        .get("reward_address")
                        .and_then(|v| v.as_str())
                        .unwrap_or("N/A")
                );
                println!(
                    "  Tier:           {}",
                    result.get("tier").and_then(|v| v.as_str()).unwrap_or("N/A")
                );
                println!(
                    "  Total Uptime:   {}",
                    result
                        .get("total_uptime")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                );
                println!(
                    "  Active:         {}",
                    result
                        .get("is_active")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                );
            } else {
                println!(
                    "  Message:        {}",
                    result.get("message").and_then(|v| v.as_str()).unwrap_or("")
                );
            }
        }
        Commands::Masternode {
            subcmd: MasternodeCommands::Genkey,
        } => {
            if let Some(key) = result.as_str() {
                println!("{}", key);
            } else {
                println!("{}", serde_json::to_string_pretty(result)?);
            }
        }
        _ => {
            // For commands without specific formatting, fall back to pretty JSON
            println!("{}", serde_json::to_string_pretty(result)?);
        }
    }
    Ok(())
}
