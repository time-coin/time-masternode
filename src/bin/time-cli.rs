use clap::{Parser, Subcommand};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    sendtoaddress          Send TIME to an address
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
    masternoderegister     Register a new masternode with locked collateral
    masternodeunlock       Unlock masternode collateral and deregister
    listlockedcollaterals  List all locked collaterals
  Mempool
    getmempoolinfo         Get memory pool information
    getrawmempool          Get raw memory pool
  Consensus
    getconsensusinfo       Get consensus information
  Utility
    validateaddress        Validate an address
    stop                   Stop the daemon
    uptime                 Get daemon uptime
    reindextransactions    Rebuild transaction index
  
    help                   Print this message or the help of the given subcommand(s)

{options}{after-help}
")]
struct Args {
    /// RPC server address
    #[arg(short, long, default_value = "http://127.0.0.1:24101")]
    rpc_url: String,

    /// Output compact JSON (single line)
    #[arg(long)]
    compact: bool,

    /// Output human-readable format
    #[arg(long)]
    human: bool,

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

    /// Send TIME to an address
    #[command(next_help_heading = "Wallet")]
    SendToAddress {
        /// Recipient address
        address: String,
        /// Amount to send (in TIME)
        amount: f64,
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
    /// Get masternode information (connected only by default)
    #[command(next_help_heading = "Masternode")]
    MasternodeList {
        /// Show all masternodes including disconnected
        #[arg(long)]
        all: bool,
    },

    /// Get masternode status
    #[command(next_help_heading = "Masternode")]
    MasternodeStatus,

    /// Register a new masternode with locked collateral
    #[command(next_help_heading = "Masternode")]
    MasternodeRegister {
        /// Masternode tier (bronze, silver, gold)
        tier: String,
        /// Collateral transaction ID (hex)
        collateral_txid: String,
        /// Collateral output index
        vout: u32,
        /// Reward address for masternode payments
        reward_address: String,
        /// Node address/identifier
        node_address: String,
    },

    /// Unlock masternode collateral and deregister
    #[command(next_help_heading = "Masternode")]
    MasternodeUnlock {
        /// Node address (optional, uses local if not provided)
        node_address: Option<String>,
    },

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

    // ============================================================
    // CONSENSUS COMMANDS
    // ============================================================
    /// Get consensus information
    #[command(next_help_heading = "Consensus")]
    GetConsensusInfo,

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

    /// Rebuild transaction index
    #[command(next_help_heading = "Utility")]
    ReindexTransactions,

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

async fn run_command(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();

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
        Commands::ListReceivedByAddress {
            minconf,
            include_empty,
        } => ("listreceivedbyaddress", json!([minconf, include_empty])),
        Commands::MasternodeList { all } => ("masternodelist", json!([all])),
        Commands::MasternodeStatus => ("masternodestatus", json!([])),
        Commands::MasternodeRegister {
            tier,
            collateral_txid,
            vout,
            reward_address,
            node_address,
        } => (
            "masternoderegister",
            json!([tier, collateral_txid, vout, reward_address, node_address]),
        ),
        Commands::MasternodeUnlock { node_address } => (
            "masternodeunlock",
            if let Some(addr) = node_address {
                json!([addr])
            } else {
                json!([])
            },
        ),
        Commands::ListLockedCollaterals => ("listlockedcollaterals", json!([])),
        Commands::GetConsensusInfo => ("getconsensusinfo", json!([])),
        Commands::ValidateAddress { address } => ("validateaddress", json!([address])),
        Commands::Stop => ("stop", json!([])),
        Commands::Uptime => ("uptime", json!([])),
        Commands::ReindexTransactions => ("reindextransactions", json!([])),
        Commands::CleanupLockedUTXOs => ("cleanuplockedutxos", json!([])),
        Commands::ListLockedUTXOs => ("listlockedutxos", json!([])),
        Commands::UnlockUTXO { txid, vout } => ("unlockutxo", json!([txid, vout])),
        Commands::UnlockOrphanedUTXOs => ("unlockorphanedutxos", json!([])),
        Commands::ForceUnlockAll => ("forceunlockall", json!([])),
        Commands::GetMempoolInfo => ("getmempoolinfo", json!([])),
        Commands::GetRawMempool { verbose } => ("getrawmempool", json!([verbose])),
        Commands::SendToAddress { address, amount } => ("sendtoaddress", json!([address, amount])),
        Commands::MergeUtxos {
            min_count,
            max_count,
            address,
        } => ("mergeutxos", json!([min_count, max_count, address])),
    };

    let request = RpcRequest {
        jsonrpc: "2.0".to_string(),
        id: "time-cli".to_string(),
        method: method.to_string(),
        params,
    };

    let response = client.post(&args.rpc_url).json(&request).send().await?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }

    let rpc_response: RpcResponse = response.json().await?;

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
        Commands::MasternodeRegister { .. } => {
            println!("🎉 Masternode Registration Successful!");
            println!();
            println!(
                "  Result:          {}",
                result
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Node Address:    {}",
                result
                    .get("masternode_address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Reward Address:  {}",
                result
                    .get("reward_address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Tier:            {}",
                result.get("tier").and_then(|v| v.as_str()).unwrap_or("N/A")
            );
            println!(
                "  Collateral:      {} TIME",
                result
                    .get("collateral")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
            println!(
                "  Collateral UTXO: {}",
                result
                    .get("collateral_outpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Locked at Height: {}",
                result
                    .get("locked_at_height")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
            println!(
                "  Public Key:      {}",
                result
                    .get("public_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!();
            println!("⚠️  IMPORTANT - SAVE THIS SIGNING KEY SECURELY:");
            println!(
                "  Signing Key:     {}",
                result
                    .get("signing_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!();
            println!(
                "  Message:         {}",
                result.get("message").and_then(|v| v.as_str()).unwrap_or("")
            );
        }
        Commands::MasternodeUnlock { .. } => {
            println!("✅ Masternode Deregistered Successfully!");
            println!();
            println!(
                "  Result:          {}",
                result
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Node Address:    {}",
                result
                    .get("masternode_address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Collateral UTXO: {}",
                result
                    .get("collateral_outpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A")
            );
            println!(
                "  Message:         {}",
                result.get("message").and_then(|v| v.as_str()).unwrap_or("")
            );
        }
        _ => {
            // For commands without specific formatting, fall back to pretty JSON
            println!("{}", serde_json::to_string_pretty(result)?);
        }
    }
    Ok(())
}
