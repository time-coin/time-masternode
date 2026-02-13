//! Time synchronization module for clock drift detection
//! Note: NTP synchronization is scaffolding for production deployment

#![allow(dead_code)]

use chrono::Utc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

const NTP_SERVERS: &[&str] = &[
    "time.google.com:123",
    "time.cloudflare.com:123",
    "pool.ntp.org:123",
    "time.nist.gov:123",
];

const CHECK_INTERVAL_SECONDS: u64 = 5 * 60; // 5 minutes (more frequent checks)
const MAX_DEVIATION_WARNING: i64 = 5; // 5 seconds - warn if approaching limit
const MAX_DEVIATION_SHUTDOWN: i64 = 10; // 10 seconds - spec requires ±10s tolerance

pub struct TimeSync {
    calibration_delay_ms: i64,
}

impl TimeSync {
    pub fn new() -> Self {
        Self {
            calibration_delay_ms: 0,
        }
    }

    /// Start the background NTP sync task
    pub fn start_sync_task(self) {
        tokio::spawn(async move {
            let mut sync = self;
            info!("⏰ Starting NTP time synchronization (checks every 5 minutes)");

            loop {
                if let Err(e) = sync.check_time_sync().await {
                    error!("❌ NTP sync error: {}", e);
                }
                sleep(Duration::from_secs(CHECK_INTERVAL_SECONDS)).await;
            }
        });
    }

    pub async fn check_time_sync(&mut self) -> Result<i64, String> {
        // Query multiple servers for consensus
        let mut results = Vec::new();
        let mut errors = Vec::new();

        for server in NTP_SERVERS {
            match self.query_ntp_server(server).await {
                Ok((ntp_time, ping_ms)) => {
                    let local_time = Utc::now().timestamp();
                    let deviation = ntp_time - local_time;
                    results.push((server, ntp_time, deviation, ping_ms));
                }
                Err(e) => {
                    warn!("Failed to query {}: {}", server, e);
                    errors.push(format!("{}: {}", server, e));
                }
            }
        }

        // Need at least 2 servers for consensus (or 1 if only 1 responded)
        if results.is_empty() {
            return Err(format!("All NTP servers failed: {}", errors.join(", ")));
        }

        // Calculate median deviation for robustness against outliers
        let mut deviations: Vec<i64> = results.iter().map(|(_, _, dev, _)| *dev).collect();
        deviations.sort_unstable();
        let median_deviation = if deviations.len() % 2 == 0 {
            let mid = deviations.len() / 2;
            (deviations[mid - 1] + deviations[mid]) / 2
        } else {
            deviations[deviations.len() / 2]
        };

        // Find result closest to median for reporting
        let (best_server, _, _, best_ping) = results
            .iter()
            .min_by_key(|(_, _, dev, _)| (dev - median_deviation).abs())
            .unwrap();

        // Update calibration delay
        self.calibration_delay_ms = best_ping / 2;

        let offset_ms = median_deviation * 1000;

        debug!(
            "✓ NTP sync: {} servers | Median offset: {}s | Best: {} ({}ms)",
            results.len(),
            median_deviation,
            best_server,
            best_ping
        );

        // Check deviation against strict ±10s tolerance
        if median_deviation.abs() > MAX_DEVIATION_SHUTDOWN {
            error!("");
            error!("╔════════════════════════════════════════════════════════════════╗");
            error!("║          🛑 CRITICAL: SYSTEM CLOCK OUT OF SYNC 🛑             ║");
            error!("╚════════════════════════════════════════════════════════════════╝");
            error!("");
            error!(
                "Your system clock is {}s off (tolerance: ±{}s)",
                median_deviation, MAX_DEVIATION_SHUTDOWN
            );
            error!("Protocol requires ±10s clock synchronization (§20.1)");
            error!("");
            error!("🔧 ACTION REQUIRED: Synchronize your system clock");
            error!("");
            error!("   Linux/Ubuntu:");
            error!("     sudo systemctl restart systemd-timesyncd");
            error!("     sudo timedatectl set-ntp true");
            error!("");
            error!("   macOS:");
            error!("     sudo sntp -sS time.apple.com");
            error!("");
            error!("   Windows:");
            error!("     net stop w32time && net start w32time");
            error!("     w32tm /resync");
            error!("");
            error!(
                "NTP servers queried: {} successful, {} failed",
                results.len(),
                errors.len()
            );
            error!("Median deviation: {}s", median_deviation);
            error!("");
            error!("Node shutting down to prevent consensus failures.");
            error!("");
            std::process::exit(1);
        } else if median_deviation.abs() >= MAX_DEVIATION_WARNING {
            warn!(
                "⚠️  WARNING: System time deviation is {}s (≥{} seconds)",
                median_deviation, MAX_DEVIATION_WARNING
            );
            warn!("⚠️  Clock approaching ±10s tolerance limit!");
            warn!("⚠️  Please synchronize your system clock immediately!");
        }

        Ok(offset_ms)
    }

    async fn query_ntp_server(&self, server: &str) -> Result<(i64, i64), String> {
        use std::time::Instant;
        use tokio::net::UdpSocket;

        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| format!("Failed to bind socket: {}", e))?;

        socket
            .connect(server)
            .await
            .map_err(|e| format!("Failed to connect to {}: {}", server, e))?;

        // NTP packet (48 bytes, version 3, client mode)
        let mut request = [0u8; 48];
        request[0] = 0x1B; // LI=0, VN=3, Mode=3 (client)

        let start = Instant::now();

        socket
            .send(&request)
            .await
            .map_err(|e| format!("Failed to send NTP request: {}", e))?;

        let mut response = [0u8; 48];

        // Set a timeout for receiving
        let receive_result =
            tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut response)).await;

        let ping_ms = start.elapsed().as_millis() as i64;

        receive_result
            .map_err(|_| "NTP request timed out".to_string())?
            .map_err(|e| format!("Failed to receive NTP response: {}", e))?;

        // Parse NTP timestamp from bytes 40-47 (transmit timestamp)
        let seconds = u32::from_be_bytes([response[40], response[41], response[42], response[43]]);

        // NTP epoch is Jan 1, 1900; Unix epoch is Jan 1, 1970
        // Difference is 2208988800 seconds
        const NTP_UNIX_OFFSET: i64 = 2208988800;
        let ntp_time = seconds as i64 - NTP_UNIX_OFFSET;

        Ok((ntp_time, ping_ms))
    }

    /// Get the current calibrated time (local time + calibration offset)
    #[allow(dead_code)]
    pub fn get_calibrated_time(&self) -> i64 {
        Utc::now().timestamp() + (self.calibration_delay_ms / 1000)
    }
}

impl Default for TimeSync {
    fn default() -> Self {
        Self::new()
    }
}
