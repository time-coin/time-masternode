use chrono::{DateTime, Utc};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
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
#[derive(Debug, Deserialize)]
struct BlockchainInfo {
    chain: String,
    blocks: u64,
    best_blockhash: String,
}

#[derive(Debug, Deserialize)]
struct WalletInfo {
    balance: f64,
    address: String,
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
    tier: String,
    address: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    collateral: f64,
}

#[derive(Debug, Deserialize)]
struct ConsensusInfo {
    #[serde(default)]
    finalized_height: u64,
    #[serde(default)]
    active_validators: usize,
}

#[derive(Debug, Deserialize)]
struct MempoolInfo {
    size: usize,
    bytes: u64,
}

struct DashboardData {
    blockchain: Option<BlockchainInfo>,
    wallet: Option<WalletInfo>,
    network: Option<NetworkInfo>,
    peers: Vec<PeerInfo>,
    masternode: Option<MasternodeStatus>,
    consensus: Option<ConsensusInfo>,
    mempool: Option<MempoolInfo>,
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
        }
    }

    async fn update_data(&mut self) {
        self.error_message = None;

        // Fetch blockchain info
        if let Ok(info) = self
            .rpc_call::<BlockchainInfo>("getblockchaininfo", &[])
            .await
        {
            self.data.blockchain = Some(info);
        }

        // Fetch wallet info
        if let Ok(info) = self.rpc_call::<WalletInfo>("getwalletinfo", &[]).await {
            self.data.wallet = Some(info);
        }

        // Fetch network info
        if let Ok(info) = self.rpc_call::<NetworkInfo>("getnetworkinfo", &[]).await {
            self.data.network = Some(info);
        }

        // Fetch peer info
        if let Ok(peers) = self.rpc_call::<Vec<PeerInfo>>("getpeerinfo", &[]).await {
            self.data.peers = peers;
        }

        // Fetch masternode status
        if let Ok(status) = self
            .rpc_call::<MasternodeStatus>("masternodestatus", &[])
            .await
        {
            self.data.masternode = Some(status);
        }

        // Fetch consensus info
        if let Ok(info) = self
            .rpc_call::<ConsensusInfo>("getconsensusinfo", &[])
            .await
        {
            self.data.consensus = Some(info);
        }

        // Fetch mempool info
        if let Ok(info) = self.rpc_call::<MempoolInfo>("getmempoolinfo", &[]).await {
            self.data.mempool = Some(info);
        }

        self.data.last_update = Utc::now();
        self.data.update_count += 1;
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: &[serde_json::Value],
    ) -> Result<T, Box<dyn Error>> {
        #[derive(Serialize)]
        struct RpcRequest<'a> {
            jsonrpc: &'a str,
            id: u32,
            method: &'a str,
            params: &'a [serde_json::Value],
        }

        #[derive(Deserialize)]
        struct RpcResponse<T> {
            result: Option<T>,
            error: Option<serde_json::Value>,
        }

        let request = RpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };

        let response = self
            .client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await?;

        let rpc_response: RpcResponse<T> = response.json().await?;

        if let Some(error) = rpc_response.error {
            return Err(format!("RPC error: {}", error).into());
        }

        rpc_response
            .result
            .ok_or_else(|| "No result in response".into())
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
            "TIME Coin Masternode Dashboard",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::styled(
            format!("Height: {}", block_height),
            Style::default().fg(Color::Green),
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
            Constraint::Length(10), // Blockchain info
            Constraint::Length(8),  // Wallet info
            Constraint::Min(0),     // Consensus info
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
                Span::raw("Best Block Hash: "),
                Span::styled(&bc.best_blockhash[..16], Style::default().fg(Color::Gray)),
                Span::raw("..."),
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
                Span::raw("Address: "),
                Span::styled(&wallet.address, Style::default().fg(Color::Cyan)),
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
                Span::raw("Finalized Height: "),
                Span::styled(
                    format!("{}", consensus.finalized_height),
                    Style::default().fg(Color::Green),
                ),
            ]),
            Line::from(vec![
                Span::raw("Active Validators: "),
                Span::styled(
                    format!("{}", consensus.active_validators),
                    Style::default().fg(Color::Yellow),
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

    // Peer list
    let peers: Vec<Row> = app
        .data
        .peers
        .iter()
        .take(20)
        .map(|peer| {
            let ping = peer
                .pingtime
                .map(|p| format!("{:.0} ms", p * 1000.0))
                .unwrap_or_else(|| "N/A".to_string());
            let direction = if peer.inbound { "←" } else { "→" };

            Row::new(vec![
                Cell::from(direction),
                Cell::from(peer.addr.clone()),
                Cell::from(ping),
            ])
        })
        .collect();

    let peer_table = Table::new(
        peers,
        [
            Constraint::Length(3),
            Constraint::Min(30),
            Constraint::Length(12),
        ],
    )
    .header(Row::new(vec!["Dir", "Address", "Ping"]).style(Style::default().fg(Color::Yellow)))
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
        let status_color = if mn.active { Color::Green } else { Color::Red };

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
                Span::raw("Address: "),
                Span::styled(&mn.address, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::raw("Active: "),
                Span::styled(
                    if mn.active { "Yes" } else { "No" },
                    Style::default().fg(status_color),
                ),
            ]),
            Line::from(vec![
                Span::raw("Collateral: "),
                Span::styled(
                    format!("{:.8} TIME", mn.collateral),
                    Style::default().fg(Color::Green),
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
    if let Some(mempool) = &app.data.mempool {
        let info = vec![
            Line::from(vec![
                Span::raw("Transactions: "),
                Span::styled(
                    format!("{}", mempool.size),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
            Line::from(vec![
                Span::raw("Size: "),
                Span::styled(
                    format!("{:.2} KB", mempool.bytes as f64 / 1024.0),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ];

        let block = Paragraph::new(info)
            .block(Block::default().borders(Borders::ALL).title("Mempool"))
            .style(Style::default().fg(Color::White));
        f.render_widget(block, area);
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

async fn detect_network() -> String {
    let client = Client::new();

    // Try both ports and check which network they're running
    for (url, expected_network) in [
        ("http://127.0.0.1:24101", "testnet"),
        ("http://127.0.0.1:24001", "mainnet"),
    ] {
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
                // Parse the response to verify the network type
                if let Ok(rpc_response) = response.json::<serde_json::Value>().await {
                    if let Some(result) = rpc_response.get("result") {
                        if let Some(chain) = result.get("chain").and_then(|c| c.as_str()) {
                            // Verify the chain matches what we expect for this port
                            if chain.to_lowercase() == expected_network {
                                return url.to_string();
                            }
                        } else {
                            // If no chain field, assume it's correct for this port
                            return url.to_string();
                        }
                    }
                }
            }
        }
    }

    // Default to testnet if neither responds
    "http://127.0.0.1:24101".to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse command line arguments or auto-detect network
    let rpc_url = if let Some(url) = std::env::args().nth(1) {
        url
    } else {
        // Auto-detect network type by checking which port responds
        detect_network().await
    };

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
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
                            app.should_quit = true;
                        }
                        KeyCode::Char('r') => {
                            app.update_data().await;
                            last_update = Instant::now();
                        }
                        KeyCode::Tab | KeyCode::Right => {
                            app.next_tab();
                        }
                        KeyCode::Left => {
                            app.previous_tab();
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
