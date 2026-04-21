//! Separate attack event log.
//!
//! Writes a structured plaintext entry to `<data_dir>/attacks.log` for every
//! AI-detected attack pattern.  The log is append-only and human-readable,
//! intended for operator monitoring and incident post-mortems.
//!
//! The file is created on first write; if the directory does not exist the
//! write fails silently (a warning is emitted to the main debug log).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::ai::attack_detector::AttackPattern;

pub struct AttackLog {
    path: PathBuf,
}

impl AttackLog {
    /// Create a new `AttackLog` that writes to `<data_dir>/attacks.log`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("attacks.log"),
        }
    }

    /// Append a single attack event to the log file.
    pub async fn log(&self, attack: &AttackPattern) {
        self.log_all(std::slice::from_ref(attack)).await;
    }

    /// Append multiple attack events in a single write (used by the enforcement task).
    pub async fn log_all(&self, attacks: &[AttackPattern]) {
        if attacks.is_empty() {
            return;
        }
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut lines = String::with_capacity(attacks.len() * 120);
        for attack in attacks {
            lines.push_str(&format!(
                "[{}] type={:?} severity={:?} confidence={:.0}% ips=[{}] action={:?}\n",
                ts,
                attack.attack_type,
                attack.severity,
                attack.confidence * 100.0,
                attack.source_ips.join(","),
                attack.recommended_action,
            ));
        }
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(mut f) => {
                    if let Err(e) = f.write_all(lines.as_bytes()) {
                        tracing::warn!("⚠️ Attack log write error: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "⚠️ Attack log open failed ({}) — {}",
                        path.display(),
                        e
                    );
                }
            }
        })
        .await
        .ok();
    }
}
