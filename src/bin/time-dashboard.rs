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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs},
    Frame, Terminal,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};

const DASHBOARD_VERSION: &str = "1.0.0";
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

#[derive(Debug, Clone, Deserialize)]
struct MempoolTx {
    txid: String,
    status: String,
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
}

struct DashboardData {
    blockchain: Option<BlockchainInfo>,
    wallet: Option<WalletInfo>,
    network: Option<NetworkInfo>,
    peers: Vec<PeerInfo>,
    masternode: Option<MasternodeStatus>,
    consensus: Option<ConsensusInfo>,
    mempool: Option<MempoolInfo>,
    mempool_txs: Vec<MempoolTx>,
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
            consensus: None,
            mempool: None,
            mempool_txs: Vec::new(),
            last_update: Utc::now(),
            update_count: 0,
        }
    }
}

struct App {
    data: DashboardData,
    rpc_url: String,
    client: Client,
    current_tab: usize,
    should_quit: bool,
    error_message: Option<String>,
    mempool_scroll: usize,
    mempool_detail: Option<usize>,
}

impl App {
    fn new(rpc_url: String) -> Self {
        Self {
            data: DashboardData::default(),
            rpc_url,
            client: Client::new(),
            current_tab: 0,
            should_quit: false,
            error_message: None,
            mempool_scroll: 0,
            mempool_detail: None,
        }
    }

    async fn update_data(&mut self) {
        self.error_message = None;

        // Fetch blockchain info
        match self
            .rpc_call::<BlockchainInfo>("getblockchaininfo", vec![])
            .await
        {
            Ok(info) => self.data.blockchain = Some(info),
            Err(e) => {
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

        self.data.last_update = Utc::now();
        self.data.update_count += 1;
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

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await?;

        // Get raw response text first for debugging
        let response_text = response.text().await?;

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
        self.current_tab = (self.current_tab + 1) % 4;
    }

    fn previous_tab(&mut self) {
        if self.current_tab > 0 {
            self.current_tab -= 1;
        } else {
            self.current_tab = 3;
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
        _ => {}
    }

    // Footer
    render_footer(f, chunks[3], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let block_height = app.data.blockchain.as_ref().map(|b| b.blocks).unwrap_or(0);
    let connections = app
        .data
        .network
        .as_ref()
        .map(|n| n.connections)
        .unwrap_or(0);
    let mn_status = app
        .data
        .masternode
        .as_ref()
        .map(|m| m.status.as_str())
        .unwrap_or("Unknown");

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
            format!("{}", block_height),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("Peers: {}", connections),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("Status: {}", mn_status),
            Style::default().fg(if mn_status == "Active" {
                Color::Green
            } else {
                Color::Red
            }),
        ),
    ])])
    .block(Block::default().borders(Borders::ALL))
    .alignment(Alignment::Left);

    f.render_widget(header, area);
}

fn render_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles = vec!["Overview", "Network", "Masternode", "Mempool"];
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
            Constraint::Length(8), // Blockchain info
            Constraint::Length(7), // Wallet info
            Constraint::Length(8), // Consensus info
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

    // Network info
    if let Some(network) = &app.data.network {
        let info = vec![
            Line::from(vec![
                Span::raw("Version: "),
                Span::styled(
                    format!("{}", network.version),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::raw("Subversion: "),
                Span::styled(&network.subversion, Style::default().fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::raw("Connections: "),
                Span::styled(
                    format!("{}", network.connections),
                    Style::default().fg(Color::Green),
                ),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title("Network"))
            .style(Style::default().fg(Color::White));
        f.render_widget(block, chunks[0]);
    }

    // Peer list — sorted by fastest ping, numbered
    let mut sorted_peers: Vec<&PeerInfo> = app.data.peers.iter().collect();
    sorted_peers.sort_by(|a, b| {
        let pa = a.pingtime.unwrap_or(f64::MAX);
        let pb = b.pingtime.unwrap_or(f64::MAX);
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let peers: Vec<Row> = sorted_peers
        .iter()
        .take(20)
        .enumerate()
        .map(|(i, peer)| {
            let ping = peer
                .pingtime
                .map(|p| format!("{:.0} ms", p * 1000.0))
                .unwrap_or_else(|| "N/A".to_string());
            let direction = if peer.inbound { "←" } else { "→" };

            Row::new(vec![
                Cell::from(format!("{}", i + 1)),
                Cell::from(direction),
                Cell::from(peer.addr.clone()),
                Cell::from(ping),
            ])
        })
        .collect();

    let peer_table = Table::new(
        peers,
        [
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(30),
            Constraint::Length(12),
        ],
    )
    .header(Row::new(vec!["#", "Dir", "Address", "Ping"]).style(Style::default().fg(Color::Yellow)))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Connected Peers ({})", app.data.peers.len())),
    )
    .style(Style::default().fg(Color::White));

    f.render_widget(peer_table, chunks[1]);
}

fn render_masternode(f: &mut Frame, area: Rect, app: &App) {
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
                    .title("Masternode Details"),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(block, area);
    } else {
        let text = Paragraph::new("No masternode registered on this node")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Masternode Details"),
            )
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center);
        f.render_widget(text, area);
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
            let selected = idx == app.mempool_scroll;
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

fn render_mempool_detail(f: &mut Frame, area: Rect, tx: &MempoolTx) {
    let status_color = if tx.status == "finalized" {
        Color::Green
    } else {
        Color::Magenta
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("TxID: ", Style::default().fg(Color::Yellow)),
            Span::styled(&tx.txid, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                tx.status.to_uppercase(),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Amount: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME", tx.amount),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Fee:    ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{:.8} TIME ({} sats)", tx.fee_time, tx.fee),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Inputs:  ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{}", tx.inputs), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Outputs: ", Style::default().fg(Color::Yellow)),
            Span::styled(format!("{}", tx.outputs), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Size:    ", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("{} bytes", tx.size),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Age:     ", Style::default().fg(Color::Yellow)),
            Span::styled(format_age(tx.age_secs), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("To: ", Style::default().fg(Color::Yellow)),
            Span::styled(&tx.to, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter or Esc to go back",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Transaction Detail"),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(block, area);
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

async fn detect_network(prefer_testnet: bool) -> String {
    let client = Client::new();

    // Try preferred network first
    let ports: Vec<(&str, &str)> = if prefer_testnet {
        vec![
            ("http://127.0.0.1:24101", "testnet"),
            ("http://127.0.0.1:24001", "mainnet"),
        ]
    } else {
        vec![
            ("http://127.0.0.1:24001", "mainnet"),
            ("http://127.0.0.1:24101", "testnet"),
        ]
    };

    for (url, expected_network) in ports {
        if let Ok(response) = client
            .post(url)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getblockchaininfo",
                "params": []
            }))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if response.status().is_success() {
                if let Ok(rpc_response) = response.json::<serde_json::Value>().await {
                    if let Some(result) = rpc_response.get("result") {
                        if let Some(chain) = result.get("chain").and_then(|c| c.as_str()) {
                            if chain.to_lowercase() == expected_network {
                                return url.to_string();
                            }
                        } else {
                            return url.to_string();
                        }
                    }
                }
            }
        }
    }

    // Default based on preference
    if prefer_testnet {
        "http://127.0.0.1:24101".to_string()
    } else {
        "http://127.0.0.1:24001".to_string()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let testnet = args.iter().any(|a| a == "--testnet");

    // Parse command line arguments or auto-detect network
    let rpc_url = if let Some(url) = args.iter().find(|a| a.starts_with("http")) {
        url.clone()
    } else {
        // Auto-detect: try mainnet first unless --testnet flag is set
        detect_network(testnet).await
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(rpc_url);

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
                        KeyCode::Char('q') => {
                            if app.mempool_detail.is_some() {
                                app.mempool_detail = None;
                            } else {
                                app.should_quit = true;
                            }
                        }
                        KeyCode::Char('r') => {
                            app.update_data().await;
                            last_update = Instant::now();
                        }
                        KeyCode::Tab | KeyCode::Right => {
                            if app.mempool_detail.is_none() {
                                app.next_tab();
                                app.mempool_scroll = 0;
                            }
                        }
                        KeyCode::Left => {
                            if app.mempool_detail.is_none() {
                                app.previous_tab();
                                app.mempool_scroll = 0;
                            }
                        }
                        KeyCode::Up => {
                            if app.current_tab == 3 && app.mempool_detail.is_none() {
                                app.mempool_scroll = app.mempool_scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Down => {
                            if app.current_tab == 3 && app.mempool_detail.is_none() {
                                let max = app.data.mempool_txs.len().saturating_sub(1);
                                if app.mempool_scroll < max {
                                    app.mempool_scroll += 1;
                                }
                            }
                        }
                        KeyCode::Enter => {
                            if app.current_tab == 3 {
                                if app.mempool_detail.is_some() {
                                    app.mempool_detail = None;
                                } else if !app.data.mempool_txs.is_empty() {
                                    app.mempool_detail = Some(app.mempool_scroll);
                                }
                            }
                        }
                        KeyCode::Esc => {
                            if app.mempool_detail.is_some() {
                                app.mempool_detail = None;
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
