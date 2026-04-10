use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Get git commit hash
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Get total git commit count — used in handshake so peers can detect outdated nodes
    let git_commit_count: u32 = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    // Get build date using stdlib only (no chrono build-dep)
    let build_date = {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        unix_secs_to_utc_string(secs)
    };

    // Set environment variables for build
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=GIT_COMMIT_COUNT={}", git_commit_count);
    println!("cargo:rustc-env=BUILD_DATE={}", build_date);

    // Only rerun when HEAD or refs change (not on every source file edit)
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
}

/// Convert Unix seconds to "YYYY-MM-DD HH:MM UTC" without external crates.
fn unix_secs_to_utc_string(secs: u64) -> String {
    let days_since_epoch = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;

    // Gregorian calendar conversion (algorithm from Richards 2013)
    let j = days_since_epoch + 2440588; // Julian Day Number for Unix epoch
    let f = j + 1401 + (((4 * j + 274277) / 146097) * 3) / 4 - 38;
    let e = 4 * f + 3;
    let g = (e % 1461) / 4;
    let h = 5 * g + 2;

    let day = (h % 153) / 5 + 1;
    let month = (h / 153 + 2) % 12 + 1;
    let year = e / 1461 - 4716 + (14 - month) / 12;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        year, month, day, hour, minute
    )
}
