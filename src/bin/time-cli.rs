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

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
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

    /// Get masternode information
    MasternodeList,

    /// Get masternode status
    MasternodeStatus,

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
        Commands::GetBalance => ("getbalance", json!([])),
        Commands::ListUnspent { minconf, maxconf } => ("listunspent", json!([minconf, maxconf])),
        Commands::MasternodeList => ("masternodelist", json!([])),
        Commands::MasternodeStatus => ("masternodestatus", json!([])),
        Commands::GetConsensusInfo => ("getconsensusinfo", json!([])),
        Commands::ValidateAddress { address } => ("validateaddress", json!([address])),
        Commands::Stop => ("stop", json!([])),
        Commands::Uptime => ("uptime", json!([])),
        Commands::GetMempoolInfo => ("getmempoolinfo", json!([])),
        Commands::GetRawMempool { verbose } => ("getrawmempool", json!([verbose])),
        Commands::SendToAddress { address, amount } => ("sendtoaddress", json!([address, amount])),
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
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}
