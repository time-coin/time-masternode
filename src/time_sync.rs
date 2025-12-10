use chrono::Utc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

const NTP_SERVERS: &[&str] = &[
    "time.google.com:123",
    "time.cloudflare.com:123",
    "pool.ntp.org:123",
    "time.nist.gov:123",
];

const CHECK_INTERVAL_SECONDS: u64 = 30 * 60; // 30 minutes
const MAX_DEVIATION_WARNING: i64 = 60; // 1 minute in seconds
const MAX_DEVIATION_SHUTDOWN: i64 = 120; // 2 minutes in seconds

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
            info!("⏰ Starting NTP time synchronization (checks every 30 minutes)");

            loop {
                if let Err(e) = sync.check_time_sync().await {
                    error!("❌ NTP sync error: {}", e);
                }
                sleep(Duration::from_secs(CHECK_INTERVAL_SECONDS)).await;
            }
        });
    }

    async fn check_time_sync(&mut self) -> Result<(), String> {
        let mut last_error = None;

        // Try each NTP server until one succeeds
        for server in NTP_SERVERS {
            match self.query_ntp_server(server).await {
                Ok((ntp_time, ping_ms)) => {
                    let local_time = Utc::now().timestamp();
                    let deviation = ntp_time - local_time;

                    // Update calibration delay (half of round-trip time)
                    self.calibration_delay_ms = ping_ms / 2;

                    info!(
                        "✓ NTP sync with {} | Offset: {}s | Ping: {}ms | Calibration: {}ms",
                        server, deviation, ping_ms, self.calibration_delay_ms
                    );

                    // Check deviation
                    if deviation.abs() >= MAX_DEVIATION_SHUTDOWN {
                        error!(
                            "🛑 CRITICAL: System time deviation is {}s (>{} seconds)",
                            deviation, MAX_DEVIATION_SHUTDOWN
                        );
                        error!("🛑 Local time: {} | NTP time: {}", local_time, ntp_time);
                        error!("🛑 Shutting down to prevent consensus issues");
                        std::process::exit(1);
                    } else if deviation.abs() >= MAX_DEVIATION_WARNING {
                        warn!(
                            "⚠️  WARNING: System time deviation is {}s (>{} seconds)",
                            deviation, MAX_DEVIATION_WARNING
                        );
                        warn!("⚠️  Please synchronize your system clock!");
                    }

                    return Ok(());
                }
                Err(e) => {
                    last_error = Some(format!("{}: {}", server, e));
                    continue;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| "All NTP servers failed".to_string()))
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
    pub fn get_calibrated_time(&self) -> i64 {
        Utc::now().timestamp() + (self.calibration_delay_ms / 1000)
    }
}

impl Default for TimeSync {
    fn default() -> Self {
        Self::new()
    }
}
