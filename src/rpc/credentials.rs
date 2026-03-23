//! Shared RPC credential resolution for CLI tools (time-cli, time-dashboard).
//!
//! Credential priority: CLI flags > .cookie file > time.conf

use std::path::PathBuf;

/// Get the data directory for the given network.
fn data_dir(testnet: bool) -> Option<PathBuf> {
    let base = dirs::home_dir()?.join(".timecoin");
    Some(if testnet { base.join("testnet") } else { base })
}

/// Read RPC credentials from the `.cookie` file.
pub fn read_cookie_file(testnet: bool) -> Option<(String, String)> {
    let cookie_path = data_dir(testnet)?.join(".cookie");
    let contents = std::fs::read_to_string(cookie_path).ok()?;
    let (user, pass) = contents.trim().split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// Read RPC credentials from `time.conf`.
pub fn read_conf_credentials(testnet: bool) -> Option<(String, String)> {
    let conf_path = data_dir(testnet)?.join("time.conf");
    let contents = std::fs::read_to_string(conf_path).ok()?;
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

/// Read the `rpctls` setting from `time.conf` (defaults to `true`).
pub fn read_conf_rpctls(testnet: bool) -> bool {
    let conf_path = match data_dir(testnet) {
        Some(d) => d.join("time.conf"),
        None => return true,
    };
    let contents = match std::fs::read_to_string(conf_path) {
        Ok(c) => c,
        Err(_) => return true,
    };
    let mut rpctls = true;
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

/// Resolve credentials: .cookie file first, then time.conf, then empty.
pub fn resolve_credentials(testnet: bool) -> (String, String) {
    read_cookie_file(testnet)
        .or_else(|| read_conf_credentials(testnet))
        .unwrap_or_default()
}
