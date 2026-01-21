use clap::{Parser, Subcommand};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Parser, Debug)]
#[command(name = "time-cli")]
#[command(about = "TIME Coin CLI - Bitcoin-like RPC client", long_about = None)]
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
    /// Get blockchain information
    GetBlockchainInfo,

    /// Get information about a specific block
    GetBlock {
        /// Block height or hash
        height: u64,
    },

    /// Get the current block count
    GetBlockCount,

    /// Get the hash of the best (tip) block
    GetBestBlockHash,

    /// Get block hash at a given height
    GetBlockHash {
        /// Block height
        height: u64,
    },

    /// Get network information
    GetNetworkInfo,

    /// Get peer information
    GetPeerInfo,

    /// Get information about the UTXO set
    GetTxOutSetInfo,

    /// Get information about a transaction
    GetTransaction {
        /// Transaction ID (hex)
        txid: String,
    },

    /// Get raw transaction data
    GetRawTransaction {
        /// Transaction ID (hex)
        txid: String,
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// Send a raw transaction
    SendRawTransaction {
        /// Hex-encoded transaction
        hex: String,
    },

    /// Create a new transaction
    CreateRawTransaction {
        /// JSON array of inputs
        inputs: String,
        /// JSON object of outputs
        outputs: String,
    },

    /// Decode a raw transaction
    DecodeRawTransaction {
        /// Hex-encoded transaction
        hex: String,
    },

    /// Get wallet balance
    GetBalance,

    /// List unspent transaction outputs
    ListUnspent {
        /// Minimum confirmations
        #[arg(default_value = "1")]
        minconf: u32,
        /// Maximum confirmations
        #[arg(default_value = "9999999")]
        maxconf: u32,
    },

    /// Get a new receiving address
    GetNewAddress,

    /// Get wallet information
    GetWalletInfo,

    /// Get masternode information
    MasternodeList,

    /// Get masternode status
    MasternodeStatus,

    /// Register a new masternode with locked collateral
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
    MasternodeUnlock {
        /// Node address (optional, uses local if not provided)
        node_address: Option<String>,
    },

    /// List all locked collaterals
    ListLockedCollaterals,

    /// Get consensus information
    GetConsensusInfo,

    /// Validate an address
    ValidateAddress {
        /// Address to validate
        address: String,
    },

    /// Stop the daemon
    Stop,

    /// Get daemon uptime
    Uptime,

    /// Get memory pool information
    GetMempoolInfo,

    /// Get raw memory pool
    GetRawMempool {
        /// Verbose output
        #[arg(short, long)]
        verbose: bool,
    },

    /// Send TIME to an address
    SendToAddress {
        /// Recipient address
        address: String,
        /// Amount to send (in TIME)
        amount: f64,
    },

    /// Merge UTXOs to reduce UTXO set size
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
        Commands::ListUnspent { minconf, maxconf } => ("listunspent", json!([minconf, maxconf])),
        Commands::GetNewAddress => ("getnewaddress", json!([])),
        Commands::GetWalletInfo => ("getwalletinfo", json!([])),
        Commands::MasternodeList => ("masternodelist", json!([])),
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
        Commands::ListUnspent { .. } => {
            if let Some(utxos) = result.as_array() {
                println!("Unspent Transaction Outputs:");
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
                println!("\nTotal UTXOs: {}", utxos.len());
            }
        }
        Commands::MasternodeList => {
            if let Some(obj) = result.as_object() {
                if let Some(nodes) = obj.get("masternodes").and_then(|v| v.as_array()) {
                    println!("Masternodes:");
                    println!(
                        "{:<42} {:<10} {:<8} {:<12} {:<12}",
                        "Address", "Tier", "Active", "Uptime", "Collateral"
                    );
                    println!("{}", "-".repeat(90));
                    for node in nodes {
                        let address = node.get("address").and_then(|v| v.as_str()).unwrap_or("");
                        let tier = node.get("tier").and_then(|v| v.as_str()).unwrap_or("");
                        let active = node
                            .get("is_active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
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
                            "{:<42} {:<10} {:<8} {:<12} {:<12}",
                            address, tier, active, uptime, collateral_status
                        );
                    }
                    println!("\nTotal Masternodes: {}", nodes.len());
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
