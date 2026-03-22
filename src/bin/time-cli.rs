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
    getblock               Get information about a specific block
    getblockcount          Get the current block count
    getbestblockhash       Get the hash of the best (tip) block
    getblockhash           Get block hash at a given height
    gettxoutsetinfo        Get information about the UTXO set
  Network
    getnetworkinfo         Get network information
    getpeerinfo            Get peer information
  Wallet
    getbalance             Get wallet balance
    getwalletinfo          Get wallet information
    getnewaddress          Get a new receiving address
    listreceivedbyaddress  List addresses with balances
    listunspent            List unspent transaction outputs
    listtransactions       List recent wallet transactions
    sendtoaddress          Send TIME to an address
    sendfrom               Send TIME from a specific address
    mergeutxos             Merge UTXOs to reduce UTXO set size
  Transaction
    gettransaction         Get information about a transaction
    getrawtransaction      Get raw transaction data
    createrawtransaction   Create a new transaction
    decoderawtransaction   Decode a raw transaction
    sendrawtransaction     Send a raw transaction
  Masternode
    masternodelist         Get masternode information
    masternodestatus       Get masternode status
    listlockedcollaterals  List all locked collaterals
  Mempool
    getmempoolinfo         Get memory pool information
    getrawmempool          Get raw memory pool
    getmempoolverbose      Get detailed mempool transactions
  Consensus
    getconsensusinfo       Get consensus information
  Treasury
    gettreasurybalance     Get on-chain treasury balance
  Utility
    validateaddress        Validate an address
    stop                   Stop the daemon
    uptime                 Get daemon uptime
    getinfo                Get general node information
    reindextransactions    Rebuild transaction index
    reindex                Full reindex: rebuild UTXOs + tx index from block 0

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

    /// Get information about the UTXO set
    #[command(next_help_heading = "Blockchain")]
    GetTxOutSetInfo,

    // ============================================================
    // NETWORK COMMANDS
    // ============================================================
    /// Get network information
    #[command(next_help_heading = "Network")]
    GetNetworkInfo,

    /// Get peer information
    #[command(next_help_heading = "Network")]
    GetPeerInfo,

    // ============================================================
    // WALLET COMMANDS
    // ============================================================
    /// Get wallet balance
    #[command(next_help_heading = "Wallet")]
    GetBalance,

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

    /// List all locked collaterals
    #[command(next_help_heading = "Masternode")]
    ListLockedCollaterals,

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

    // ============================================================
    // TREASURY COMMANDS
    // ============================================================
    /// Get the on-chain treasury balance
    #[command(next_help_heading = "Treasury")]
    GetTreasuryBalance,

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

    /// Rebuild transaction index
    #[command(next_help_heading = "Utility")]
    ReindexTransactions,

    /// Full reindex: rebuild UTXOs and transaction index from block 0
    #[command(next_help_heading = "Utility")]
    Reindex,

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

/// Read RPC credentials from the .cookie file in the data directory.
/// Read rpctls setting from time.conf (defaults to true, matching server default).
fn read_conf_rpctls(testnet: bool) -> bool {
    let data_dir = if testnet {
        match dirs::home_dir() {
            Some(d) => d.join(".timecoin").join("testnet"),
            None => return true,
        }
    } else {
        match dirs::home_dir() {
            Some(d) => d.join(".timecoin"),
            None => return true,
        }
    };
    let conf_path = data_dir.join("time.conf");
    let contents = match std::fs::read_to_string(&conf_path) {
        Ok(c) => c,
        Err(_) => return true, // default: TLS on
    };
    let mut rpctls = true; // server default
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "rpctls" {
                rpctls = value.trim() != "0";
            }
        }
    }
    rpctls
}

fn read_cookie_file(testnet: bool) -> Option<(String, String)> {
    let data_dir = if testnet {
        dirs::home_dir()?.join(".timecoin").join("testnet")
    } else {
        dirs::home_dir()?.join(".timecoin")
    };
    let cookie_path = data_dir.join(".cookie");
    let contents = std::fs::read_to_string(&cookie_path).ok()?;
    let (user, pass) = contents.trim().split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Read RPC credentials from time.conf as a fallback.
fn read_conf_credentials(testnet: bool) -> Option<(String, String)> {
    let data_dir = if testnet {
        dirs::home_dir()?.join(".timecoin").join("testnet")
    } else {
        dirs::home_dir()?.join(".timecoin")
    };
    let conf_path = data_dir.join("time.conf");
    let contents = std::fs::read_to_string(&conf_path).ok()?;
    let mut user = None;
    let mut pass = None;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "rpcuser" => user = Some(value.trim().to_string()),
                "rpcpassword" => pass = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    Some((user?, pass?))
}

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

    let (method, params) = match &args.command {
        Commands::GetBlockchainInfo => ("getblockchaininfo", json!([])),
        Commands::GetBlock { height } => ("getblock", json!([height])),
        Commands::GetBlockCount => ("getblockcount", json!([])),
        Commands::GetBestBlockHash => ("getbestblockhash", json!([])),
        Commands::GetBlockHash { height } => ("getblockhash", json!([height])),
        Commands::GetNetworkInfo => ("getnetworkinfo", json!([])),
        Commands::GetPeerInfo => ("getpeerinfo", json!([])),
        Commands::GetTxOutSetInfo => ("gettxoutsetinfo", json!([])),
        Commands::GetTransaction { txid } => ("gettransaction", json!([txid])),
        Commands::GetRawTransaction { txid, verbose } => {
            ("getrawtransaction", json!([txid, verbose]))
        }
        Commands::SendRawTransaction { hex } => ("sendrawtransaction", json!([hex])),
        Commands::CreateRawTransaction { inputs, outputs } => {
            let inputs_json: Value = serde_json::from_str(inputs)?;
            let outputs_json: Value = serde_json::from_str(outputs)?;
            ("createrawtransaction", json!([inputs_json, outputs_json]))
        }
        Commands::DecodeRawTransaction { hex } => ("decoderawtransaction", json!([hex])),
        Commands::GetBalance => ("getbalance", json!([])),
        Commands::ListUnspent {
            limit,
            minconf,
            maxconf,
        } => ("listunspent", json!([minconf, maxconf, null, limit])),
        Commands::GetNewAddress => ("getnewaddress", json!([])),
        Commands::GetWalletInfo => ("getwalletinfo", json!([])),
        Commands::ListTransactions { count } => ("listtransactions", json!([count])),
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
        Commands::ListLockedCollaterals => ("listlockedcollaterals", json!([])),
        Commands::GetConsensusInfo => ("getconsensusinfo", json!([])),
        Commands::GetTreasuryBalance => ("gettreasurybalance", json!([])),
        Commands::ValidateAddress { address } => ("validateaddress", json!([address])),
        Commands::Stop => ("stop", json!([])),
        Commands::Uptime => ("uptime", json!([])),
        Commands::GetInfo => ("getinfo", json!([])),
        Commands::ReindexTransactions => ("reindextransactions", json!([])),
        Commands::Reindex => ("reindex", json!([])),
        Commands::CleanupLockedUTXOs => ("cleanuplockedutxos", json!([])),
        Commands::ListLockedUTXOs => ("listlockedutxos", json!([])),
        Commands::UnlockUTXO { txid, vout } => ("unlockutxo", json!([txid, vout])),
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
            eprintln!("⚠️  HTTPS RPC failed ({}); retrying over plain HTTP — check rpctlscert/rpctlskey", e);
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

    let rpc_response: RpcResponse = response.json().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if let Some(error) = rpc_response.error {
        return Err(format!("RPC error {}: {}", error.code, error.message).into());
    }

    if let Some(result) = rpc_response.result {
        if args.human {
            print_human_readable(&args.command, &result)?;
        } else if args.compact {
            println!("{}", serde_json::to_string(&result)?);
        } else {
            // Default: pretty JSON (like Bitcoin)
            println!("{}", serde_json::to_string_pretty(&result)?);
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
        Commands::GetBalance => {
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
