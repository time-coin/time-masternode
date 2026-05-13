use chrono::{DateTime, Utc};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};
use timed::http_client::HttpClient;

const DASHBOARD_VERSION: &str = "1.0.0";

use timed::rpc::credentials::resolve_credentials;

/// Check ~/.timecoin/time.conf for testnet=1 to auto-detect network preference.
fn conf_prefers_testnet() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let conf_path = home.join(".timecoin").join("time.conf");
    let Ok(contents) = std::fs::read_to_string(&conf_path) else {
        return false;
    };
    contents.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line == "testnet=1"
    })
}

/// Detect which network is running by checking which data directory has a live
/// .cookie file. Falls back to time.conf then defaults to mainnet.
fn detect_running_network() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let mainnet_cookie = home.join(".timecoin").join(".cookie");
    let testnet_cookie = home.join(".timecoin").join("testnet").join(".cookie");

    let mainnet_exists = mainnet_cookie.exists();
    let testnet_exists = testnet_cookie.exists();

    match (mainnet_exists, testnet_exists) {
        (true, false) => false, // only mainnet cookie → mainnet
        (false, true) => true,  // only testnet cookie → testnet
        (true, true) => {
            // Both running; prefer whichever cookie is newer
            let mt = std::fs::metadata(&mainnet_cookie)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let tt = std::fs::metadata(&testnet_cookie)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            tt > mt // testnet cookie is newer
        }
        (false, false) => conf_prefers_testnet(), // no cookies, fall back to config
    }
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BlockchainInfo {
    chain: String,
    blocks: u64,
    headers: u64,
    bestblockhash: String,
    difficulty: f64,
    mediantime: u64,
    verificationprogress: f64,
    chainwork: String,
    pruned: bool,
    consensus: String,
    finality_mechanism: String,
    instant_finality: bool,
    average_finality_time_ms: u64,
    block_time_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct WalletInfo {
    balance: f64,
    #[serde(default)]
    locked: f64,
    #[serde(default)]
    available: f64,
    #[serde(default)]
    #[allow(dead_code)]
    txcount: usize,
}

#[derive(Debug, Deserialize)]
struct NetworkInfo {
    version: u32,
    subversion: String,
    connections: usize,
}

#[derive(Debug, Deserialize)]
struct PeerInfo {
    addr: String,
    #[serde(default)]
    pingtime: Option<f64>,
    #[serde(default)]
    inbound: bool,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    height: u64,
}

#[derive(Debug, Deserialize)]
struct MasternodeStatus {
    status: String,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    reward_address: String,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    total_uptime: u64,
    #[serde(default)]
    version: String,
    #[serde(default)]
    git_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct MasternodeListEntry {
    address: String,
    wallet_address: String,
    tier: String,
    is_active: bool,
    is_connected: bool,
    #[serde(default)]
    is_publicly_reachable: bool,
    collateral: f64,
    total_uptime: u64,
    #[serde(default)]
    daemon_started_at: u64,
}

#[derive(Debug, Deserialize)]
struct MasternodeList {
    total: usize,
    total_in_registry: usize,
    masternodes: Vec<MasternodeListEntry>,
}

#[derive(Debug, Deserialize)]
struct ConsensusInfo {
    protocol: String,
    #[serde(default)]
    active_validators: usize,
    #[serde(default)]
    instant_finality: bool,
    #[serde(default)]
    average_finality_time_ms: u64,
}

#[derive(Debug, Deserialize)]
struct MempoolInfo {
    size: usize,
    bytes: usize,
    #[serde(default)]
    pending: usize,
    #[serde(default)]
    finalized: usize,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TxVin {
    #[serde(default)]
    txid: String,
    #[serde(default)]
    vout: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TxVout {
    #[serde(default)]
    value: f64,
    #[serde(default)]
    n: usize,
    #[serde(default)]
    address: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TxDetail {
    #[serde(default)]
    txid: String,
    #[serde(default)]
    #[allow(dead_code)]
    status: String,
    #[serde(default)]
    amount: f64,
    #[serde(default)]
    fee: f64,
    #[serde(default)]
    size: usize,
    #[serde(default)]
    confirmations: i64,
    #[serde(default)]
    time: u64,
    #[serde(default)]
    vin: Vec<TxVin>,
    #[serde(default)]
    vout: Vec<TxVout>,
    #[serde(default)]
    special_data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct MempoolTx {
    txid: String,
    status: String,
    #[allow(dead_code)]
    fee: u64,
    #[serde(default)]
    fee_time: f64,
    #[serde(default)]
    amount: f64,
    #[serde(default)]
    size: usize,
    #[serde(default)]
    inputs: usize,
    #[serde(default)]
    outputs: usize,
    #[serde(default)]
    age_secs: u64,
    #[serde(default)]
    to: String,
    #[serde(default)]
    vin: Vec<TxVin>,
    #[serde(default)]
    vout: Vec<TxVout>,
    #[serde(default)]
    special_data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct BlockDetail {
    height: u64,
    hash: String,
    #[serde(default)]
    previousblockhash: String,
    time: u64,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    merkleroot: String,
    #[serde(default, rename = "nTx")]
    n_tx: usize,
    #[serde(default)]
    tx: Vec<String>,
    #[serde(default)]
    confirmations: i64,
    #[serde(default)]
    block_reward: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct GovernanceProposal {
    id: String,
    #[serde(rename = "type")]
    proposal_type: String,
    submitter: String,
    submit_height: u64,
    vote_end_height: u64,
    status: String,
    #[serde(default)]
    total_weight: u64,
}

struct DashboardData {
    blockchain: Option<BlockchainInfo>,
    wallet: Option<WalletInfo>,
    network: Option<NetworkInfo>,
    peers: Vec<PeerInfo>,
    masternode: Option<MasternodeStatus>,
    masternode_list: Option<MasternodeList>,
    consensus: Option<ConsensusInfo>,
    mempool: Option<MempoolInfo>,
    mempool_txs: Vec<MempoolTx>,
    recent_blocks: Vec<BlockDetail>,
    proposals: Vec<GovernanceProposal>,
    last_update: DateTime<Utc>,
    update_count: u64,
}

impl Default for DashboardData {
    fn default() -> Self {
        Self {
            blockchain: None,
            wallet: None,
            network: None,
            peers: Vec::new(),
            masternode: None,
            masternode_list: None,
            consensus: None,
            mempool: None,
            mempool_txs: Vec::new(),
            recent_blocks: Vec::new(),
            proposals: Vec::new(),
            last_update: Utc::now(),
            update_count: 0,
        }
    }
}

struct App {
    data: DashboardData,
    rpc_url: String,
    rpc_user: String,
    rpc_pass: String,
    client: HttpClient,
    current_tab: usize,
    should_quit: bool,
    rpc_connected: bool,
    error_message: Option<String>,
    mempool_scroll: usize,
    mempool_cursor: usize,
    mempool_detail: Option<usize>,
    peer_scroll: usize,
    block_scroll: usize,
    block_detail: Option<usize>,
    block_tx_scroll: usize,
    block_tx_cursor: usize,
    block_tx_detail: Option<TxDetail>,
    governance_scroll: usize,
    masternode_scroll: usize,
    vote_status: Option<(bool, String)>, // (success, message)
}

impl App {
    fn new(rpc_url: String, rpc_user: String, rpc_pass: String) -> Self {
        Self {
            data: DashboardData::default(),
            rpc_url,
            rpc_user,
            rpc_pass,
            client: HttpClient::new()
                .with_timeout(Duration::from_secs(3))
                .with_accept_invalid_certs(true),
            current_tab: 0,
            should_quit: false,
            rpc_connected: false,
            error_message: None,
            mempool_scroll: 0,
            mempool_cursor: 0,
            mempool_detail: None,
            peer_scroll: 0,
            block_scroll: 0,
            block_detail: None,
            block_tx_scroll: 0,
            block_tx_cursor: 0,
            block_tx_detail: None,
            governance_scroll: 0,
            masternode_scroll: 0,
            vote_status: None,
        }
    }

    async fn update_data(&mut self) {
        self.error_message = None;

        // Fetch blockchain info — first call determines RPC reachability
        match self
            .rpc_call::<BlockchainInfo>("getblockchaininfo", vec![])
            .await
        {
            Ok(info) => {
                self.rpc_connected = true;
                self.data.blockchain = Some(info);
            }
            Err(e) => {
                self.rpc_connected = false;
                self.error_message = Some(format!("getblockchaininfo: {}", e));
                return;
            }
        }

        // Fetch wallet info
        match self.rpc_call::<WalletInfo>("getwalletinfo", vec![]).await {
            Ok(info) => self.data.wallet = Some(info),
            Err(e) => self.error_message = Some(format!("getwalletinfo: {}", e)),
        }

        // Fetch network info
        match self.rpc_call::<NetworkInfo>("getnetworkinfo", vec![]).await {
            Ok(info) => self.data.network = Some(info),
            Err(e) => self.error_message = Some(format!("getnetworkinfo: {}", e)),
        }

        // Fetch peer info
        match self.rpc_call::<Vec<PeerInfo>>("getpeerinfo", vec![]).await {
            Ok(peers) => self.data.peers = peers,
            Err(e) => self.error_message = Some(format!("getpeerinfo: {}", e)),
        }

        // Fetch masternode status
        match self
            .rpc_call::<MasternodeStatus>("masternodestatus", vec![])
            .await
        {
            Ok(status) => self.data.masternode = Some(status),
            Err(e) => self.error_message = Some(format!("masternodestatus: {}", e)),
        }

        // Fetch network masternode list
        if let Ok(list) = self
            .rpc_call::<MasternodeList>("masternodelist", vec![serde_json::json!(true)])
            .await
        {
            self.data.masternode_list = Some(list);
        }

        // Fetch consensus info
        match self
            .rpc_call::<ConsensusInfo>("getconsensusinfo", vec![])
            .await
        {
            Ok(info) => self.data.consensus = Some(info),
            Err(e) => self.error_message = Some(format!("getconsensusinfo: {}", e)),
        }

        // Fetch mempool info
        match self.rpc_call::<MempoolInfo>("getmempoolinfo", vec![]).await {
            Ok(info) => self.data.mempool = Some(info),
            Err(e) => self.error_message = Some(format!("getmempoolinfo: {}", e)),
        }

        // Fetch verbose mempool transactions
        match self
            .rpc_call::<Vec<MempoolTx>>("getmempoolverbose", vec![])
            .await
        {
            Ok(txs) => self.data.mempool_txs = txs,
            Err(_) => self.data.mempool_txs = Vec::new(),
        }

        // Fetch recent blocks for block explorer
        if let Some(bc) = &self.data.blockchain {
            let current_height = bc.blocks;
            let best_hash = bc.bestblockhash.clone();
            let cached_top = self
                .data
                .recent_blocks
                .first()
                .map(|b| b.height)
                .unwrap_or(0);
            let cached_top_hash = self
                .data
                .recent_blocks
                .first()
                .map(|b| b.hash.clone())
                .unwrap_or_default();

            // Detect reorg: best block hash changed for the same (or lower) height
            let reorg_detected = !self.data.recent_blocks.is_empty()
                && (current_height < cached_top
                    || (current_height == cached_top && cached_top_hash != best_hash)
                    || (current_height > cached_top && {
                        // The cached tip may be on a stale fork; verify its hash still matches
                        // what the node reports at that height by checking if the top cached
                        // block's hash is still the best known at that height.
                        // We detect this cheaply: fetch the block at cached_top and compare.
                        matches!(
                            self.rpc_call::<BlockDetail>(
                                "getblock",
                                vec![serde_json::json!(cached_top)]
                            )
                            .await,
                            Ok(ref live) if live.hash != cached_top_hash
                        )
                    }));

            if reorg_detected {
                // Chain reorganized — wipe stale cache and do a full re-fetch
                self.data.recent_blocks.clear();
            }

            let cached_top = self
                .data
                .recent_blocks
                .first()
                .map(|b| b.height)
                .unwrap_or(0);

            if cached_top < current_height || self.data.recent_blocks.is_empty() {
                // Fetch only new blocks since last cached height
                let fetch_from = if cached_top == 0 {
                    current_height.saturating_sub(19) // initial: last 20 blocks
                } else {
                    cached_top + 1
                };

                let mut new_blocks = Vec::new();
                for h in (fetch_from..=current_height).rev() {
                    match self
                        .rpc_call::<BlockDetail>("getblock", vec![serde_json::json!(h)])
                        .await
                    {
                        Ok(block) => new_blocks.push(block),
                        Err(_) => break,
                    }
                }

                if !new_blocks.is_empty() {
                    // Prepend new blocks (newest first) and cap at 50
                    new_blocks.append(&mut self.data.recent_blocks);
                    new_blocks.truncate(50);
                    self.data.recent_blocks = new_blocks;
                }
            }
        }

        // Fetch governance proposals
        if let Ok(proposals) = self
            .rpc_call::<Vec<GovernanceProposal>>("listproposals", vec![])
            .await
        {
            self.data.proposals = proposals;
            // governance may not be initialized yet — silently ignore errors
        }

        // Insert local node as first entry in peer list
        if let Some(ref mn) = self.data.masternode {
            // Remove the local node from the RPC peer list to avoid duplication
            if !mn.address.is_empty() {
                let local_ip = mn.address.split(':').next().unwrap_or(&mn.address);
                self.data.peers.retain(|p| {
                    let peer_ip = p.addr.split(':').next().unwrap_or(&p.addr);
                    peer_ip != local_ip
                });
            }
            let local_height = self.data.blockchain.as_ref().map(|b| b.blocks).unwrap_or(0);
            let local_peer = PeerInfo {
                addr: if mn.address.is_empty() {
                    "(this node)".to_string()
                } else {
                    format!("{} (you)", mn.address)
                },
                pingtime: None,
                inbound: false,
                tier: mn.tier.clone(),
                active: mn.is_active,
                height: local_height,
            };
            self.data.peers.insert(0, local_peer);
        }

        self.data.last_update = Utc::now();
        self.data.update_count += 1;
    }

    async fn cast_vote(&self, proposal_id: &str, approve: bool) -> Result<String, String> {
        let params = vec![serde_json::json!(proposal_id), serde_json::json!(approve)];
        match self
            .rpc_call::<serde_json::Value>("voteproposal", params)
            .await
        {
            Ok(_) => Ok(format!(
                "Vote {} recorded for {}",
                if approve { "YES" } else { "NO" },
                &proposal_id[..16.min(proposal_id.len())]
            )),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Vec<serde_json::Value>,
    ) -> Result<T, Box<dyn Error>> {
        #[derive(Serialize)]
        struct RpcRequest {
            jsonrpc: String,
            id: String,
            method: String,
            params: Vec<serde_json::Value>,
        }

        let request = RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: "1".to_string(),
            method: method.to_string(),
            params,
        };

        let auth = if !self.rpc_user.is_empty() && !self.rpc_pass.is_empty() {
            Some((self.rpc_user.as_str(), self.rpc_pass.as_str()))
        } else {
            None
        };
        let response = self
            .client
            .post_json(&self.rpc_url, &request, auth)
            .await
            .map_err(|e| -> Box<dyn Error> { e.into() })?;

        // Get raw response text for debugging
        let response_text = response.text();

        // First parse as generic Value to check for errors
        let rpc_value: serde_json::Value = serde_json::from_str(&response_text)?;

        // Check for RPC error
        if let Some(error) = rpc_value.get("error") {
            if !error.is_null() {
                return Err(format!("RPC error: {}", error).into());
            }
        }

        // Extract result field
        let result = rpc_value.get("result").ok_or("No result in RPC response")?;

        // Deserialize the result into our target type
        serde_json::from_value(result.clone()).map_err(|e| {
            eprintln!("DEBUG: Failed to parse {} result", method);
            eprintln!("DEBUG: Error: {}", e);
            eprintln!(
                "DEBUG: Result JSON: {}",
                serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
            );
            format!("Failed to deserialize {}: {}", method, e).into()
        })
    }

    fn next_tab(&mut self) {
        self.current_tab = (self.current_tab + 1) % 6;
    }

    fn previous_tab(&mut self) {
        if self.current_tab > 0 {
            self.current_tab -= 1;
        } else {
            self.current_tab = 5;
        }
    }

    /// Returns true if this node is eligible to participate in governance
    /// (Gold, Silver, or Bronze masternode). Free tier and non-masternodes
    /// are excluded.
    fn can_govern(&self) -> bool {
        match &self.data.masternode {
            None => false,
            Some(mn) => {
                let tier = mn.tier.to_lowercase();
                matches!(tier.as_str(), "gold" | "silver" | "bronze")
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(3), // Tabs
            Constraint::Min(0),    // Content
            Constraint::Length(3), // Footer
        ])
        .split(f.size());

    // Header
    render_header(f, chunks[0], app);

    // Tabs
    render_tabs(f, chunks[1], app);

    // Content based on current tab
    match app.current_tab {
        0 => render_overview(f, chunks[2], app),
        1 => render_network(f, chunks[2], app),
        2 => render_masternode(f, chunks[2], app),
        3 => render_mempool(f, chunks[2], app),
        4 => render_blocks(f, chunks[2], app),
        5 => render_governance(f, chunks[2], app),
        _ => {}
    }

    // Footer
    render_footer(f, chunks[3], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let offline = !app.rpc_connected;

    // When offline, show stale-data markers so the header clearly reflects
    // the disconnected state rather than silently showing old values.
    let height_text = if offline {
        "---".to_string()
    } else {
        app.data
            .blockchain
            .as_ref()
            .map(|b| b.blocks.to_string())
            .unwrap_or_else(|| "---".to_string())
    };

    let peers_text = if offline {
        "---".to_string()
    } else {
        app.data
            .network
            .as_ref()
            .map(|n| n.connections.to_string())
            .unwrap_or_else(|| "---".to_string())
    };

    let (status_text, status_color) = if offline {
        ("Status: Node Offline".to_string(), Color::Red)
    } else {
        match app.data.masternode.as_ref() {
            Some(mn) if mn.is_active => ("Status: Active".to_string(), Color::Green),
            Some(mn) => (
                format!(
                    "Status: {}",
                    if mn.status.is_empty() {
                        "Inactive"
                    } else {
                        &mn.status
                    }
                ),
                Color::Yellow,
            ),
            None => ("Status: Unknown".to_string(), Color::Yellow),
        }
    };

    let header = Paragraph::new(vec![Line::from(vec![
        Span::styled(
            format!("TIME Coin Masternode Dashboard v{}", DASHBOARD_VERSION),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::raw("Height: "),
        Span::styled(
            height_text,
            Style::default()
                .fg(if offline {
                    Color::DarkGray
                } else {
                    Color::White
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("Peers: {}", peers_text),
            Style::default().fg(if offline {
                Color::DarkGray
            } else {
                Color::Yellow
            }),
        ),
        Span::raw("  |  "),
        Span::styled(
            status_text,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ])])
    .block(Block::default().borders(Borders::ALL))
    .alignment(Alignment::Left);

    f.render_widget(header, area);
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles = vec![
        "Overview",
        "Network",
        "Masternode",
        "Mempool",
        "Blocks",
        "Governance",
    ];
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title("Navigation"))
        .select(app.current_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(tabs, area);
}

fn render_overview(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9), // Blockchain info (5 lines + border)
            Constraint::Length(7), // Wallet info
            Constraint::Min(0),    // Consensus info
        ])
        .split(area);

    // Blockchain info
    if let Some(bc) = &app.data.blockchain {
        let info = vec![
            Line::from(vec![
                Span::raw("Chain: "),
                Span::styled(&bc.chain, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::raw("Block Height: "),
                Span::styled(format!("{}", bc.blocks), Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::raw("Best Block: "),
                Span::styled(&bc.bestblockhash[..16], Style::default().fg(Color::Gray)),
                Span::raw("..."),
            ]),
            Line::from(vec![
                Span::raw("Consensus: "),
                Span::styled(&bc.consensus, Style::default().fg(Color::Magenta)),
            ]),
            Line::from(vec![
                Span::raw("Sync Progress: "),
                Span::styled(
                    format!("{:.2}%", bc.verificationprogress * 100.0),
                    Style::default().fg(if bc.verificationprogress >= 0.999 {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title("Blockchain"))
            .style(Style::default().fg(Color::White));
        f.render_widget(block, chunks[0]);
    }

    // Wallet info
    if let Some(wallet) = &app.data.wallet {
        let info = vec![
            Line::from(vec![
                Span::raw("Balance: "),
                Span::styled(
                    format!("{:.8} TIME", wallet.balance),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("Available: "),
                Span::styled(
                    format!("{:.8} TIME", wallet.available),
                    Style::default().fg(Color::Green),
                ),
            ]),
            Line::from(vec![
                Span::raw("Locked: "),
                Span::styled(
                    format!("{:.8} TIME", wallet.locked),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title("Wallet"))
            .style(Style::default().fg(Color::White));
        f.render_widget(block, chunks[1]);
    }

    // Consensus info
    if let Some(consensus) = &app.data.consensus {
        let info = vec![
            Line::from(vec![
                Span::raw("Protocol: "),
                Span::styled(&consensus.protocol, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::raw("Active Validators: "),
                Span::styled(
                    format!("{}", consensus.active_validators),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
            Line::from(vec![
                Span::raw("Instant Finality: "),
                Span::styled(
                    if consensus.instant_finality {
                        "Yes"
                    } else {
                        "No"
                    },
                    Style::default().fg(if consensus.instant_finality {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
            ]),
            Line::from(vec![
                Span::raw("Avg Finality: "),
                Span::styled(
                    format!("{}ms", consensus.average_finality_time_ms),
                    Style::default().fg(Color::Green),
                ),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title("Consensus"))
            .style(Style::default().fg(Color::White));
        f.render_widget(block, chunks[2]);
    }
}

fn render_network(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7), // Network info
            Constraint::Min(0),    // Peer list
        ])
        .split(area);

    // Count inbound vs outbound
    let outbound_count = app.data.peers.iter().filter(|p| !p.inbound).count();
    let inbound_count = app.data.peers.iter().filter(|p| p.inbound).count();
    let active_count = app.data.peers.iter().filter(|p| p.active).count();

    if let Some(network) = &app.data.network {
        let info = vec![
            Line::from(vec![
                Span::raw("Version: "),
                Span::styled(
                    format!("{}", network.version),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw("  Subversion: "),
                Span::styled(&network.subversion, Style::default().fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::raw("Connections: "),
                Span::styled(
                    format!("{}", network.connections),
                    Style::default().fg(Color::Green),
                ),
                Span::raw("  ("),
                Span::styled(
                    format!("{} out", outbound_count),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(" / "),
                Span::styled(
                    format!("{} in", inbound_count),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(")"),
            ]),
            Line::from(vec![
                Span::raw("Active masternodes: "),
                Span::styled(
                    format!("{}", active_count),
                    Style::default().fg(Color::Green),
                ),
                Span::raw(format!(" / {} peers", app.data.peers.len())),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title("Network"))
            .style(Style::default().fg(Color::White));
        f.render_widget(block, chunks[0]);
    }

    // Peer list — sorted: local node first, then active masternodes, then by ping
    let mut sorted_peers: Vec<&PeerInfo> = app.data.peers.iter().collect();
    sorted_peers.sort_by(|a, b| {
        let a_local = a.addr.ends_with("(you)") || a.addr == "(this node)";
        let b_local = b.addr.ends_with("(you)") || b.addr == "(this node)";
        if a_local {
            return std::cmp::Ordering::Less;
        }
        if b_local {
            return std::cmp::Ordering::Greater;
        }
        // Active masternodes first
        let a_mn = !a.tier.is_empty() && a.tier != "Unknown";
        let b_mn = !b.tier.is_empty() && b.tier != "Unknown";
        match (a.active, b.active, a_mn, b_mn) {
            (true, false, _, _) => std::cmp::Ordering::Less,
            (false, true, _, _) => std::cmp::Ordering::Greater,
            _ => {
                // Among peers of same status, masternodes before non-masternodes
                match (a_mn, b_mn) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => {
                        let pa = a.pingtime.unwrap_or(f64::MAX);
                        let pb = b.pingtime.unwrap_or(f64::MAX);
                        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                    }
                }
            }
        }
    });

    let total_peers = sorted_peers.len();
    let scroll = app.peer_scroll.min(total_peers.saturating_sub(1));

    let rows: Vec<Row> = sorted_peers
        .iter()
        .enumerate()
        .map(|(i, peer)| {
            let ping = peer
                .pingtime
                .map(|p| format!("{:.0} ms", p * 1000.0))
                .unwrap_or_else(|| "—".to_string());
            let is_local = peer.addr.ends_with("(you)") || peer.addr == "(this node)";
            let direction = if is_local {
                "local"
            } else if peer.inbound {
                "← in"
            } else {
                "→ out"
            };
            let tier_display = if peer.tier.is_empty() || peer.tier == "Unknown" {
                "—".to_string()
            } else {
                peer.tier.clone()
            };
            let height_str = if peer.height > 0 {
                format!("{}", peer.height)
            } else {
                "—".to_string()
            };
            let status_marker = if peer.active { "●" } else { "○" };

            let row_style = if is_local {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if peer.active && !peer.inbound {
                Style::default().fg(Color::Green)
            } else if peer.active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            Row::new(vec![
                Cell::from(format!("{}", i + 1)),
                Cell::from(status_marker),
                Cell::from(direction),
                Cell::from(peer.addr.clone()),
                Cell::from(tier_display),
                Cell::from(height_str),
                Cell::from(ping),
            ])
            .style(row_style)
        })
        .collect();

    let mut table_state = TableState::default();
    if !rows.is_empty() {
        table_state.select(Some(scroll));
    }

    let title = format!("Connected Peers ({})  [↑↓ scroll]", total_peers);

    let peer_table = Table::new(
        rows,
        [
            Constraint::Length(4), // #
            Constraint::Length(2), // status dot
            Constraint::Length(6), // dir
            Constraint::Min(22),   // address
            Constraint::Length(8), // type
            Constraint::Length(8), // height
            Constraint::Length(9), // ping
        ],
    )
    .header(
        Row::new(vec!["#", "", "Dir", "Address", "Type", "Height", "Ping"]).style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(Block::default().borders(Borders::ALL).title(title))
    .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .style(Style::default().fg(Color::White));

    f.render_stateful_widget(peer_table, chunks[1], &mut table_state);
}

fn render_masternode(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10), // Local node details
            Constraint::Min(0),     // Network masternode list
        ])
        .split(area);

    if let Some(mn) = &app.data.masternode {
        let status_color = if mn.is_active {
            Color::Green
        } else {
            Color::Red
        };

        let uptime_hours = mn.total_uptime / 3600;
        let uptime_days = uptime_hours / 24;

        let info = vec![
            Line::from(vec![
                Span::raw("Status: "),
                Span::styled(
                    &mn.status,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("Tier: "),
                Span::styled(
                    &mn.tier,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("Node Address: "),
                Span::styled(&mn.address, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::raw("Reward Address: "),
                Span::styled(&mn.reward_address, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::raw("Uptime: "),
                Span::styled(
                    format!("{} days, {} hours", uptime_days, uptime_hours % 24),
                    Style::default().fg(Color::Green),
                ),
            ]),
            Line::from(vec![
                Span::raw("Version: "),
                Span::styled(
                    format!("v{} ({})", mn.version, mn.git_hash),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Local Masternode"),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(block, chunks[0]);
    } else {
        let text = Paragraph::new("No masternode registered on this node")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Local Masternode"),
            )
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        f.render_widget(text, chunks[0]);
    }

    // Network masternode list
    if let Some(list) = &app.data.masternode_list {
        // Sort masternodes by tier: Gold > Silver > Bronze > Free
        let tier_order = |t: &str| -> u8 {
            match t {
                "Gold" => 0,
                "Silver" => 1,
                "Bronze" => 2,
                _ => 3,
            }
        };
        let mut sorted: Vec<&MasternodeListEntry> = list.masternodes.iter().collect();
        sorted.sort_by_key(|a| tier_order(&a.tier));

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let rows: Vec<Row> = sorted
            .iter()
            .map(|mn| {
                let active_style = if mn.is_active {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Red)
                };
                let conn_style = if mn.is_connected {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let reach_style = if mn.is_publicly_reachable {
                    Style::default().fg(Color::Green)
                } else if mn.is_connected {
                    // Connected but not yet confirmed reachable (still probing or behind NAT)
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                // Use daemon_started_at for real remote uptime, fall back to local total_uptime
                let uptime_secs = if mn.daemon_started_at > 0 {
                    now_secs.saturating_sub(mn.daemon_started_at)
                } else {
                    mn.total_uptime
                };
                let uptime_mins = uptime_secs / 60;
                let uptime_hrs = uptime_secs / 3600;
                let uptime_str = if uptime_hrs >= 24 {
                    format!("{}d {}h", uptime_hrs / 24, uptime_hrs % 24)
                } else if uptime_hrs > 0 {
                    format!("{}h", uptime_hrs)
                } else {
                    format!("{}m", uptime_mins)
                };
                let tier_color = match mn.tier.as_str() {
                    "Gold" => Color::Yellow,
                    "Silver" => Color::White,
                    "Bronze" => Color::Rgb(205, 127, 50),
                    _ => Color::Cyan,
                };
                let short_addr = if mn.address.len() > 22 {
                    format!("{}…", &mn.address[..22])
                } else {
                    mn.address.clone()
                };
                Row::new(vec![
                    Cell::from(short_addr),
                    Cell::from(mn.tier.clone()).style(Style::default().fg(tier_color)),
                    Cell::from(if mn.is_active { "✓" } else { "✗" }).style(active_style),
                    Cell::from(if mn.is_connected { "✓" } else { "✗" }).style(conn_style),
                    Cell::from(if mn.is_publicly_reachable {
                        "✓"
                    } else {
                        "✗"
                    })
                    .style(reach_style),
                    Cell::from(uptime_str),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Min(24),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(9),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(vec![
                "Address",
                "Tier",
                "Active",
                "Connected",
                "Reachable",
                "Uptime",
            ])
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Network Masternodes ({}/{})  [↑↓ scroll]",
            list.total, list.total_in_registry
        )))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .style(Style::default().fg(Color::White));

        let total_mns = sorted.len();
        let scroll = app.masternode_scroll.min(total_mns.saturating_sub(1));
        let mut table_state = TableState::default();
        if !sorted.is_empty() {
            table_state.select(Some(scroll));
        }
        f.render_stateful_widget(table, chunks[1], &mut table_state);
    } else {
        let placeholder = Paragraph::new("Loading masternode list…")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Network Masternodes"),
            )
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(placeholder, chunks[1]);
    }
}

fn render_mempool(f: &mut Frame, area: Rect, app: &App) {
    // Detail view
    if let Some(idx) = app.mempool_detail {
        if let Some(tx) = app.data.mempool_txs.get(idx) {
            render_mempool_detail(f, area, tx);
            return;
        }
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(8)])
        .split(area);

    // Summary section
    let pending = app.data.mempool.as_ref().map(|m| m.pending).unwrap_or(0);
    let finalized = app.data.mempool.as_ref().map(|m| m.finalized).unwrap_or(0);
    let total = app.data.mempool.as_ref().map(|m| m.size).unwrap_or(0);
    let bytes = app.data.mempool.as_ref().map(|m| m.bytes).unwrap_or(0);

    let total_fees: f64 = app.data.mempool_txs.iter().map(|t| t.fee_time).sum();

    let summary = vec![
        Line::from(vec![
            Span::raw("Total: "),
            Span::styled(format!("{}", total), Style::default().fg(Color::Yellow)),
            Span::raw("  Pending: "),
            Span::styled(format!("{}", pending), Style::default().fg(Color::Magenta)),
            Span::raw("  Finalized: "),
            Span::styled(format!("{}", finalized), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("Size: "),
            Span::styled(
                format!("{:.2} KB", bytes as f64 / 1024.0),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("  Total Fees: "),
            Span::styled(
                format!("{:.8} TIME", total_fees),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(Span::styled(
            "↑↓ Navigate  Enter: Details  q: Quit",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let summary_block = Paragraph::new(summary)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Mempool Summary"),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(summary_block, chunks[0]);

    // Transaction list
    if app.data.mempool_txs.is_empty() {
        let empty = Paragraph::new("No transactions in mempool")
            .block(Block::default().borders(Borders::ALL).title("Transactions"))
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(empty, chunks[1]);
        return;
    }

    let header = Row::new(vec![
        Cell::from(" # "),
        Cell::from("TxID"),
        Cell::from("Status"),
        Cell::from("Amount"),
        Cell::from("Fee"),
        Cell::from("To"),
        Cell::from("Age"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let visible_height = chunks[1].height.saturating_sub(3) as usize;
    let start = app.mempool_scroll;
    let end = (start + visible_height).min(app.data.mempool_txs.len());

    let rows: Vec<Row> = app.data.mempool_txs[start..end]
        .iter()
        .enumerate()
        .map(|(i, tx)| {
            let idx = start + i;
            let selected = idx == app.mempool_cursor;
            let status_style = if tx.status == "finalized" {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Magenta)
            };
            let short_txid = if tx.txid.len() > 12 {
                format!("{}…", &tx.txid[..12])
            } else {
                tx.txid.clone()
            };
            let short_to = if tx.to.len() > 16 {
                format!("{}…", &tx.to[..16])
            } else {
                tx.to.clone()
            };
            let age = format_age(tx.age_secs);
            let row = Row::new(vec![
                Cell::from(format!("{:>3}", idx + 1)),
                Cell::from(short_txid),
                Cell::from(tx.status.clone()).style(status_style),
                Cell::from(format!("{:.4}", tx.amount)),
                Cell::from(format!("{:.4}", tx.fee_time)),
                Cell::from(short_to),
                Cell::from(age),
            ]);
            if selected {
                row.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(16),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Transactions ({}/{})",
        start + 1,
        app.data.mempool_txs.len()
    )));
    f.render_widget(table, chunks[1]);
}

/// Returns a short label like "MasternodeRegistration (node: 1.2.3.4:24000)"
/// from the `special_data` JSON, or `None` for regular transactions.
fn special_data_label(sd: &Option<serde_json::Value>) -> Option<String> {
    let sd = sd.as_ref()?;
    let ty = sd.get("type")?.as_str()?;
    let node = sd
        .get("node_address")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Some(format!("{} ({})", ty, node))
}

fn render_mempool_detail(f: &mut Frame, area: Rect, tx: &MempoolTx) {
    let status_color = if tx.status == "finalized" {
        Color::Green
    } else {
        Color::Magenta
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Min(4),
            Constraint::Min(4),
        ])
        .split(area);

    // Header summary
    let header_lines = vec![
        Line::from(vec![
            Span::styled("TxID:   ", Style::default().fg(Color::Yellow)),
            Span::styled(&tx.txid, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                tx.status.to_uppercase(),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("Size: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{} bytes", tx.size),
                Style::default().fg(Color::White),
            ),
            Span::raw("   "),
            Span::styled("Age: ", Style::default().fg(Color::Yellow)),
            Span::styled(format_age(tx.age_secs), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Amount: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME", tx.amount),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("   "),
            Span::styled("Fee: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME", tx.fee_time),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Esc / Enter to go back",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let header = Paragraph::new(header_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Transaction Detail"),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(header, chunks[0]);

    // Inputs
    let input_header = Row::new(vec![
        Cell::from(" # "),
        Cell::from("Prev TxID"),
        Cell::from("Vout"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let special_label = special_data_label(&tx.special_data);
    let input_rows: Vec<Row> = if tx.vin.is_empty() {
        let label = special_label.as_deref().unwrap_or("(coinbase)");
        vec![Row::new(vec![
            Cell::from(""),
            Cell::from(label).style(Style::default().fg(Color::Magenta)),
            Cell::from(""),
        ])]
    } else {
        tx.vin
            .iter()
            .enumerate()
            .map(|(i, inp)| {
                let short = if inp.txid.len() > 20 {
                    format!("{}…", &inp.txid[..20])
                } else {
                    inp.txid.clone()
                };
                Row::new(vec![
                    Cell::from(format!("{:>3}", i + 1)),
                    Cell::from(short).style(Style::default().fg(Color::Gray)),
                    Cell::from(format!("{}", inp.vout)).style(Style::default().fg(Color::White)),
                ])
            })
            .collect()
    };
    let input_table = Table::new(
        input_rows,
        [
            Constraint::Length(4),
            Constraint::Min(22),
            Constraint::Length(6),
        ],
    )
    .header(input_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Inputs ({})", tx.inputs)),
    );
    f.render_widget(input_table, chunks[1]);

    // Outputs
    let output_header = Row::new(vec![
        Cell::from(" # "),
        Cell::from("Address"),
        Cell::from("Amount (TIME)"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let output_rows: Vec<Row> = if tx.vout.is_empty() {
        let label = if special_label.is_some() {
            "(masternode op — no outputs)"
        } else {
            "(no outputs)"
        };
        vec![Row::new(vec![
            Cell::from(""),
            Cell::from(label).style(Style::default().fg(Color::DarkGray)),
            Cell::from(""),
        ])]
    } else {
        tx.vout
            .iter()
            .enumerate()
            .map(|(i, out)| {
                let short_addr = if out.address.len() > 28 {
                    format!("{}…", &out.address[..28])
                } else {
                    out.address.clone()
                };
                Row::new(vec![
                    Cell::from(format!("{:>3}", i)),
                    Cell::from(short_addr).style(Style::default().fg(Color::Cyan)),
                    Cell::from(format!("{:.8}", out.value))
                        .style(Style::default().fg(Color::Green)),
                ])
            })
            .collect()
    };
    let output_table = Table::new(
        output_rows,
        [
            Constraint::Length(4),
            Constraint::Min(30),
            Constraint::Length(16),
        ],
    )
    .header(output_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Outputs ({})", tx.outputs)),
    );
    f.render_widget(output_table, chunks[2]);
}

fn render_blocks(f: &mut Frame, area: Rect, app: &App) {
    // TX detail drill-down (third level)
    if let Some(detail) = &app.block_tx_detail {
        render_tx_detail(f, area, detail);
        return;
    }
    // Block detail view (second level)
    if let Some(idx) = app.block_detail {
        if let Some(block) = app.data.recent_blocks.get(idx) {
            render_block_detail(f, area, block, app.block_tx_scroll, app.block_tx_cursor);
            return;
        }
    }

    if app.data.recent_blocks.is_empty() {
        let empty = Paragraph::new("No blocks available")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Block Explorer"),
            )
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(empty, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("Height"),
        Cell::from("Hash"),
        Cell::from("Time (UTC)"),
        Cell::from("Txs"),
        Cell::from("Reward"),
        Cell::from("Confirms"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let visible_height = area.height.saturating_sub(3) as usize;
    let start = app.block_scroll;
    let end = (start + visible_height).min(app.data.recent_blocks.len());

    let rows: Vec<Row> = app.data.recent_blocks[start..end]
        .iter()
        .enumerate()
        .map(|(i, blk)| {
            let idx = start + i;
            let selected = idx == app.block_scroll;
            let short_hash = if blk.hash.len() > 16 {
                format!("{}…", &blk.hash[..16])
            } else {
                blk.hash.clone()
            };
            let time_str = chrono::DateTime::from_timestamp(blk.time as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "—".to_string());

            let row = Row::new(vec![
                Cell::from(format!("{}", blk.height)),
                Cell::from(short_hash),
                Cell::from(time_str),
                Cell::from(format!("{}", blk.n_tx)),
                Cell::from(format!("{:.2}", blk.block_reward / 1e8)),
                Cell::from(format!("{}", blk.confirmations)),
            ]);
            if selected {
                row.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(18),
            Constraint::Length(18),
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Min(8),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(format!(
        "Block Explorer ({}/{})  ↑↓ Navigate  Enter: Details",
        start + 1,
        app.data.recent_blocks.len()
    )));
    f.render_widget(table, area);
}

fn render_block_detail(
    f: &mut Frame,
    area: Rect,
    blk: &BlockDetail,
    tx_scroll: usize,
    tx_cursor: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(12), Constraint::Min(6)])
        .split(area);

    let time_str = chrono::DateTime::from_timestamp(blk.time as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "—".to_string());

    let short_merkle = if blk.merkleroot.len() > 32 {
        format!("{}…", &blk.merkleroot[..32])
    } else {
        blk.merkleroot.clone()
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Height:    ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{}", blk.height),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Hash:      ", Style::default().fg(Color::Yellow)),
            Span::styled(&blk.hash, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Prev Hash: ", Style::default().fg(Color::Yellow)),
            Span::styled(&blk.previousblockhash, Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::styled("Time:      ", Style::default().fg(Color::Yellow)),
            Span::styled(time_str, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Merkle:    ", Style::default().fg(Color::Yellow)),
            Span::styled(short_merkle, Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::styled("Txs:       ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{}", blk.n_tx), Style::default().fg(Color::White)),
            Span::raw("    "),
            Span::styled("Reward: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME", blk.block_reward / 1e8),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::styled("Confirms:  ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{}", blk.confirmations),
                Style::default().fg(Color::White),
            ),
            Span::raw("    "),
            Span::styled("Version: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{}", blk.version),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "↑↓ Navigate TXs  Enter: TX Detail  Esc/q: Back",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let detail = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Block #{}", blk.height)),
    );
    f.render_widget(detail, chunks[0]);

    // Transaction list
    if blk.tx.is_empty() {
        let empty = Paragraph::new("No transactions in this block")
            .block(Block::default().borders(Borders::ALL).title("Transactions"))
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(empty, chunks[1]);
        return;
    }

    let header = Row::new(vec![Cell::from(" # "), Cell::from("Transaction ID")]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let visible = chunks[1].height.saturating_sub(3) as usize;
    let start = tx_scroll;
    let end = (start + visible).min(blk.tx.len());

    let rows: Vec<Row> = blk.tx[start..end]
        .iter()
        .enumerate()
        .map(|(i, txid)| {
            let idx = start + i;
            let row = Row::new(vec![
                Cell::from(format!("{:>3}", idx + 1)),
                Cell::from(txid.as_str()),
            ]);
            if idx == tx_cursor {
                row.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(5), Constraint::Min(40)])
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Transactions ({}/{})  Enter: View Detail",
            tx_cursor + 1,
            blk.tx.len()
        )));
    f.render_widget(table, chunks[1]);
}

fn render_tx_detail(f: &mut Frame, area: Rect, tx: &TxDetail) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(9),
            Constraint::Min(4),
            Constraint::Min(4),
        ])
        .split(area);

    let time_str = if tx.time > 0 {
        chrono::DateTime::from_timestamp(tx.time as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "—".to_string())
    } else {
        "pending".to_string()
    };

    let header_lines = vec![
        Line::from(vec![
            Span::styled("TxID:    ", Style::default().fg(Color::Yellow)),
            Span::styled(&tx.txid, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Time:    ", Style::default().fg(Color::Yellow)),
            Span::styled(&time_str, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Amount:  ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME", tx.amount),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw("   "),
            Span::styled("Fee: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME", tx.fee),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Confirms:", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!(" {}", tx.confirmations),
                Style::default().fg(Color::Green),
            ),
            Span::raw("   "),
            Span::styled("Size: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{} bytes", tx.size),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Esc/q: Back to block",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let header = Paragraph::new(header_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Transaction Detail"),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(header, chunks[0]);

    // Inputs
    let input_header = Row::new(vec![
        Cell::from(" # "),
        Cell::from("Prev TxID"),
        Cell::from("Vout"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let special_label = special_data_label(&tx.special_data);
    let input_rows: Vec<Row> = if tx.vin.is_empty() {
        let label = special_label.as_deref().unwrap_or("(coinbase)");
        vec![Row::new(vec![
            Cell::from(""),
            Cell::from(label).style(Style::default().fg(Color::Magenta)),
            Cell::from(""),
        ])]
    } else {
        tx.vin
            .iter()
            .enumerate()
            .map(|(i, inp)| {
                let short = if inp.txid.len() > 24 {
                    format!("{}…", &inp.txid[..24])
                } else {
                    inp.txid.clone()
                };
                Row::new(vec![
                    Cell::from(format!("{:>3}", i + 1)),
                    Cell::from(short).style(Style::default().fg(Color::Gray)),
                    Cell::from(format!("{}", inp.vout)).style(Style::default().fg(Color::White)),
                ])
            })
            .collect()
    };
    let input_table = Table::new(
        input_rows,
        [
            Constraint::Length(4),
            Constraint::Min(26),
            Constraint::Length(6),
        ],
    )
    .header(input_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Inputs ({})", tx.vin.len())),
    );
    f.render_widget(input_table, chunks[1]);

    // Outputs
    let output_header = Row::new(vec![
        Cell::from(" # "),
        Cell::from("Address"),
        Cell::from("Amount (TIME)"),
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let output_rows: Vec<Row> = if tx.vout.is_empty() {
        let label = if special_label.is_some() {
            "(masternode op — no outputs)"
        } else {
            "(no outputs)"
        };
        vec![Row::new(vec![
            Cell::from(""),
            Cell::from(label).style(Style::default().fg(Color::DarkGray)),
            Cell::from(""),
        ])]
    } else {
        tx.vout
            .iter()
            .map(|out| {
                let short_addr = if out.address.len() > 28 {
                    format!("{}…", &out.address[..28])
                } else {
                    out.address.clone()
                };
                Row::new(vec![
                    Cell::from(format!("{:>3}", out.n)),
                    Cell::from(short_addr).style(Style::default().fg(Color::Cyan)),
                    Cell::from(format!("{:.8}", out.value))
                        .style(Style::default().fg(Color::Green)),
                ])
            })
            .collect()
    };
    let output_table = Table::new(
        output_rows,
        [
            Constraint::Length(4),
            Constraint::Min(30),
            Constraint::Length(16),
        ],
    )
    .header(output_header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Outputs ({})", tx.vout.len())),
    );
    f.render_widget(output_table, chunks[2]);
}

fn render_governance(f: &mut Frame, area: Rect, app: &App) {
    let proposals = &app.data.proposals;

    // Layout: summary bar + vote status + proposal table + hint bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // summary
            Constraint::Length(3), // vote status / hint
            Constraint::Min(0),    // proposal table
        ])
        .split(area);

    // --- Summary bar ---
    let active = proposals.iter().filter(|p| p.status == "active").count();
    let passed = proposals
        .iter()
        .filter(|p| p.status.starts_with("passed"))
        .count();
    let failed = proposals.iter().filter(|p| p.status == "failed").count();

    let summary = Paragraph::new(Line::from(vec![
        Span::raw("Proposals: "),
        Span::styled(
            proposals.len().to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  Active: "),
        Span::styled(
            active.to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  Passed: "),
        Span::styled(passed.to_string(), Style::default().fg(Color::Green)),
        Span::raw("  |  Failed: "),
        Span::styled(failed.to_string(), Style::default().fg(Color::Red)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Governance")
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .alignment(Alignment::Left);
    f.render_widget(summary, chunks[0]);

    // --- Vote status / hint bar ---
    let eligible = app.can_govern();
    let hint_line = if !eligible {
        // Determine why: no masternode vs Free tier
        let tier = app
            .data
            .masternode
            .as_ref()
            .map(|m| m.tier.as_str())
            .unwrap_or("");
        let reason = if tier.eq_ignore_ascii_case("free") {
            "Free tier nodes cannot participate in governance."
        } else {
            "This node is not running as a masternode and cannot participate in governance."
        };
        Line::from(vec![
            Span::styled(
                "⊘ ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(reason, Style::default().fg(Color::DarkGray)),
        ])
    } else if let Some((ok, msg)) = &app.vote_status {
        let (icon, color) = if *ok {
            ("✓ ", Color::Green)
        } else {
            ("✗ ", Color::Red)
        };
        Line::from(vec![
            Span::styled(
                icon,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(msg.as_str(), Style::default().fg(color)),
        ])
    } else {
        Line::from(vec![
            Span::styled("[v] ", Style::default().fg(Color::Green)),
            Span::raw("Vote Yes  "),
            Span::styled("[x] ", Style::default().fg(Color::Red)),
            Span::raw("Vote No  "),
            Span::styled("[↑↓] ", Style::default().fg(Color::Yellow)),
            Span::raw("Navigate  "),
            Span::styled("[r] ", Style::default().fg(Color::Yellow)),
            Span::raw("Refresh"),
        ])
    };
    let hint = Paragraph::new(hint_line)
        .block(Block::default().borders(Borders::ALL))
        .alignment(Alignment::Left);
    f.render_widget(hint, chunks[1]);

    // --- Proposal table ---
    if proposals.is_empty() {
        let empty = Paragraph::new("No governance proposals found.")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Proposals")
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, chunks[2]);
        return;
    }

    let header = Row::new(vec![
        Cell::from("#").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("ID").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Type").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Submitter").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Status").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Ends At").style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .height(1);

    let rows: Vec<Row> = proposals
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let selected = i == app.governance_scroll;

            let id_short = if p.id.len() > 16 {
                format!("{}...", &p.id[..16])
            } else {
                p.id.clone()
            };

            let submitter_short = if p.submitter.len() > 14 {
                format!("{}...", &p.submitter[..14])
            } else {
                p.submitter.clone()
            };

            let type_label = match p.proposal_type.as_str() {
                "treasury_spend" => "Treasury",
                "fee_schedule_change" => "Fee Change",
                other => other,
            };

            let status_color = if p.status == "active" {
                Color::Yellow
            } else if p.status.starts_with("passed") {
                Color::Green
            } else if p.status == "failed" {
                Color::Red
            } else {
                Color::Gray
            };

            let row_style = if selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(format!("{}", i + 1)),
                Cell::from(id_short),
                Cell::from(type_label),
                Cell::from(submitter_short),
                Cell::from(p.status.clone()).style(Style::default().fg(status_color)),
                Cell::from(format!("{}", p.vote_end_height)),
            ])
            .style(row_style)
            .height(1)
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(18),
        Constraint::Length(24),
        Constraint::Length(10),
    ];

    let mut table_state = TableState::default();
    table_state.select(Some(app.governance_scroll));

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Proposals ({} total)", proposals.len()))
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(table, chunks[2], &mut table_state);
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let last_update = app.data.last_update.format("%H:%M:%S").to_string();
    let update_count = app.data.update_count;

    let footer_text = if let Some(err) = &app.error_message {
        vec![Line::from(vec![
            Span::styled(
                "ERROR: ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err, Style::default().fg(Color::Red)),
        ])]
    } else {
        vec![Line::from(vec![
            Span::raw("Last Update: "),
            Span::styled(last_update, Style::default().fg(Color::Gray)),
            Span::raw(format!(" (#{})  |  ", update_count)),
            Span::styled("[Tab/←→] ", Style::default().fg(Color::Yellow)),
            Span::raw("Switch tabs  |  "),
            Span::styled("[r] ", Style::default().fg(Color::Yellow)),
            Span::raw("Refresh  |  "),
            Span::styled("[q] ", Style::default().fg(Color::Yellow)),
            Span::raw("Quit"),
        ])]
    };

    let footer = Paragraph::new(footer_text)
        .block(Block::default().borders(Borders::ALL))
        .alignment(Alignment::Left);

    f.render_widget(footer, area);
}

async fn detect_network(prefer_testnet: bool) -> (String, bool) {
    let client = HttpClient::new()
        .with_accept_invalid_certs(true)
        .with_timeout(Duration::from_secs(2));

    // Preferred network first; for each port try https then http
    let ports: Vec<(u16, bool)> = if prefer_testnet {
        vec![(24101, true), (24001, false)]
    } else {
        vec![(24001, false), (24101, true)]
    };

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getblockchaininfo",
        "params": []
    });

    for (port, is_testnet) in &ports {
        let (user, pass) = resolve_credentials(*is_testnet);
        let auth = if !user.is_empty() && !pass.is_empty() {
            Some((user.as_str(), pass.as_str()))
        } else {
            None
        };
        for scheme in &["https", "http"] {
            let url = format!("{}://127.0.0.1:{}", scheme, port);
            if let Ok(response) = client.post_json(&url, &body, auth).await {
                if response.is_success() {
                    if let Ok(rpc_response) = response.json::<serde_json::Value>() {
                        if rpc_response.get("result").is_some() {
                            return (url, *is_testnet);
                        }
                    }
                } else if response.status == 401 || response.status == 403 {
                    // Port is alive even if credentials weren't found
                    return (url, *is_testnet);
                }
            }
        }
    }

    // Default fallback
    let scheme = "http";
    if prefer_testnet {
        (format!("{}://127.0.0.1:24101", scheme), true)
    } else {
        (format!("{}://127.0.0.1:24001", scheme), false)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    // --mainnet or "mainnet" forces mainnet, bypassing auto-detection.
    // --testnet or "testnet" forces testnet.
    // Without either flag, auto-detect from cookie files / time.conf (defaults to mainnet).
    let force_mainnet = args.iter().any(|a| a == "--mainnet" || a == "mainnet");
    let force_testnet = args.iter().any(|a| a == "--testnet" || a == "testnet");
    let prefer_testnet = if force_mainnet {
        false
    } else if force_testnet {
        true
    } else {
        detect_running_network()
    };

    // Parse command line arguments or auto-detect network
    let (rpc_url, is_testnet) = if let Some(url) = args.iter().find(|a| a.starts_with("http")) {
        let testnet = prefer_testnet || url.contains("24101");
        (url.clone(), testnet)
    } else {
        // Auto-detect: probe https then http on preferred port first
        detect_network(prefer_testnet).await
    };

    let (rpc_user, rpc_pass) = resolve_credentials(is_testnet);

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(rpc_url, rpc_user, rpc_pass);

    // Initial data fetch
    app.update_data().await;

    // Run app
    let res = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("Error: {:?}", err);
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<(), Box<dyn Error>> {
    let mut last_update = Instant::now();
    let update_interval = Duration::from_secs(2);

    loop {
        terminal.draw(|f| ui(f, app))?;

        // Handle events with timeout
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                            if app.mempool_detail.is_some() {
                                app.mempool_detail = None;
                            } else if app.block_tx_detail.is_some() {
                                app.block_tx_detail = None;
                            } else if app.block_detail.is_some() {
                                app.block_detail = None;
                                app.block_tx_scroll = 0;
                                app.block_tx_cursor = 0;
                            } else if app.current_tab == 5 && app.vote_status.is_some() {
                                app.vote_status = None;
                            } else {
                                app.should_quit = true;
                            }
                        }
                        KeyCode::Char('c')
                            if key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            app.should_quit = true;
                        }
                        KeyCode::Char('r') => {
                            app.update_data().await;
                            last_update = Instant::now();
                        }
                        KeyCode::Tab | KeyCode::Right
                            if app.mempool_detail.is_none()
                                && app.block_detail.is_none()
                                && app.block_tx_detail.is_none() =>
                        {
                            app.next_tab();
                            app.mempool_scroll = 0;
                            app.mempool_cursor = 0;
                            app.vote_status = None;
                        }
                        KeyCode::Left
                            if app.mempool_detail.is_none()
                                && app.block_detail.is_none()
                                && app.block_tx_detail.is_none() =>
                        {
                            app.previous_tab();
                            app.mempool_scroll = 0;
                            app.mempool_cursor = 0;
                            app.vote_status = None;
                        }
                        KeyCode::Up => {
                            if app.current_tab == 3 && app.mempool_detail.is_none() {
                                if app.mempool_cursor > 0 {
                                    app.mempool_cursor -= 1;
                                    // Scroll viewport up if cursor moved above it
                                    if app.mempool_cursor < app.mempool_scroll {
                                        app.mempool_scroll = app.mempool_cursor;
                                    }
                                }
                            } else if app.current_tab == 1 {
                                app.peer_scroll = app.peer_scroll.saturating_sub(1);
                            } else if app.current_tab == 2 {
                                app.masternode_scroll = app.masternode_scroll.saturating_sub(1);
                            } else if app.current_tab == 4 {
                                if app.block_tx_detail.is_some() {
                                    // nothing — no scrolling in TX detail yet
                                } else if app.block_detail.is_some() {
                                    if app.block_tx_cursor > 0 {
                                        app.block_tx_cursor -= 1;
                                        if app.block_tx_cursor < app.block_tx_scroll {
                                            app.block_tx_scroll = app.block_tx_cursor;
                                        }
                                    }
                                } else {
                                    app.block_scroll = app.block_scroll.saturating_sub(1);
                                }
                            } else if app.current_tab == 5 {
                                app.governance_scroll = app.governance_scroll.saturating_sub(1);
                                app.vote_status = None;
                            }
                        }
                        KeyCode::Down => {
                            if app.current_tab == 3 && app.mempool_detail.is_none() {
                                let max = app.data.mempool_txs.len().saturating_sub(1);
                                if app.mempool_cursor < max {
                                    app.mempool_cursor += 1;
                                    // Scroll viewport down if cursor moved below visible area
                                    // We don't have exact height here, so use a reasonable page size
                                    let page = 20usize;
                                    if app.mempool_cursor >= app.mempool_scroll + page {
                                        app.mempool_scroll =
                                            app.mempool_cursor.saturating_sub(page - 1);
                                    }
                                }
                            } else if app.current_tab == 1 {
                                let max = app.data.peers.len().saturating_sub(1);
                                if app.peer_scroll < max {
                                    app.peer_scroll += 1;
                                }
                            } else if app.current_tab == 2 {
                                let max = app
                                    .data
                                    .masternode_list
                                    .as_ref()
                                    .map(|l| l.masternodes.len())
                                    .unwrap_or(0)
                                    .saturating_sub(1);
                                if app.masternode_scroll < max {
                                    app.masternode_scroll += 1;
                                }
                            } else if app.current_tab == 4 {
                                if app.block_tx_detail.is_some() {
                                    // nothing
                                } else if let Some(detail_idx) = app.block_detail {
                                    let max_tx = app
                                        .data
                                        .recent_blocks
                                        .get(detail_idx)
                                        .map(|b| b.tx.len())
                                        .unwrap_or(0)
                                        .saturating_sub(1);
                                    if app.block_tx_cursor < max_tx {
                                        app.block_tx_cursor += 1;
                                        let page = 20usize;
                                        if app.block_tx_cursor >= app.block_tx_scroll + page {
                                            app.block_tx_scroll =
                                                app.block_tx_cursor.saturating_sub(page - 1);
                                        }
                                    }
                                } else {
                                    let max = app.data.recent_blocks.len().saturating_sub(1);
                                    if app.block_scroll < max {
                                        app.block_scroll += 1;
                                    }
                                }
                            } else if app.current_tab == 5 {
                                let max = app.data.proposals.len().saturating_sub(1);
                                if app.governance_scroll < max {
                                    app.governance_scroll += 1;
                                }
                                app.vote_status = None;
                            }
                        }
                        KeyCode::Enter => {
                            if app.current_tab == 3 {
                                if app.mempool_detail.is_some() {
                                    app.mempool_detail = None;
                                } else if !app.data.mempool_txs.is_empty() {
                                    app.mempool_detail = Some(app.mempool_cursor);
                                }
                            } else if app.current_tab == 4 {
                                if app.block_tx_detail.is_some() {
                                    app.block_tx_detail = None;
                                } else if let Some(blk_idx) = app.block_detail {
                                    // Drill into TX detail
                                    if let Some(txid) = app
                                        .data
                                        .recent_blocks
                                        .get(blk_idx)
                                        .and_then(|b| b.tx.get(app.block_tx_cursor))
                                        .cloned()
                                    {
                                        match app
                                            .rpc_call::<TxDetail>(
                                                "gettransaction",
                                                vec![serde_json::json!(txid)],
                                            )
                                            .await
                                        {
                                            Ok(mut detail) => {
                                                detail.txid = txid;
                                                app.block_tx_detail = Some(detail);
                                            }
                                            Err(_) => {
                                                // Show a minimal placeholder so user sees something
                                                app.block_tx_detail = Some(TxDetail {
                                                    txid: txid.clone(),
                                                    ..Default::default()
                                                });
                                            }
                                        }
                                    }
                                } else if !app.data.recent_blocks.is_empty() {
                                    app.block_detail = Some(app.block_scroll);
                                    app.block_tx_scroll = 0;
                                    app.block_tx_cursor = 0;
                                }
                            }
                        }
                        KeyCode::Esc => {
                            if app.mempool_detail.is_some() {
                                app.mempool_detail = None;
                            } else if app.block_tx_detail.is_some() {
                                app.block_tx_detail = None;
                            } else if app.block_detail.is_some() {
                                app.block_detail = None;
                                app.block_tx_scroll = 0;
                                app.block_tx_cursor = 0;
                            } else if app.current_tab == 5 {
                                app.vote_status = None;
                            }
                        }
                        KeyCode::Char('v') | KeyCode::Char('V') if app.current_tab == 5 => {
                            if !app.can_govern() {
                                // ignore — not eligible
                            } else if let Some(proposal) =
                                app.data.proposals.get(app.governance_scroll)
                            {
                                let id = proposal.id.clone();
                                match app.cast_vote(&id, true).await {
                                    Ok(msg) => app.vote_status = Some((true, msg)),
                                    Err(e) => app.vote_status = Some((false, e)),
                                }
                            }
                        }
                        KeyCode::Char('x') | KeyCode::Char('X') if app.current_tab == 5 => {
                            if !app.can_govern() {
                                // ignore — not eligible
                            } else if let Some(proposal) =
                                app.data.proposals.get(app.governance_scroll)
                            {
                                let id = proposal.id.clone();
                                match app.cast_vote(&id, false).await {
                                    Ok(msg) => app.vote_status = Some((true, msg)),
                                    Err(e) => app.vote_status = Some((false, e)),
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Auto-update data every 2 seconds
        if last_update.elapsed() >= update_interval {
            app.update_data().await;
            last_update = Instant::now();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
