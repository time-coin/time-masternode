use crate::error::AppError;
use sysinfo::{MemoryRefreshKind, RefreshKind, System};
use tracing::{error, info};

/// Calculate optimal cache size based on available memory
#[allow(dead_code)]
pub fn calculate_cache_size() -> u64 {
    let sys =
        System::new_with_specifics(RefreshKind::new().with_memory(MemoryRefreshKind::everything()));

    let available_memory = sys.available_memory();
    let cache_size = (available_memory / 10).min(256 * 1024 * 1024);

    info!(
        available_mb = available_memory / (1024 * 1024),
        cache_mb = cache_size / (1024 * 1024),
        "Configured sled cache"
    );

    cache_size
}

/// Open a sled database with optimized configuration
#[allow(dead_code)]
pub fn open_database(base_path: &str, name: &str, cache_size: u64) -> Result<sled::Db, AppError> {
    let path = format!("{}/{}", base_path, name);

    sled::Config::new()
        .path(&path)
        .cache_capacity(cache_size)
        .mode(sled::Mode::HighThroughput)
        .open()
        .map_err(|e| {
            error!(database = name, path = &path, "Failed to open database");
            AppError::Storage(crate::error::StorageError::DatabaseOpen {
                name: name.to_string(),
                source: e,
            })
        })
}

/// Extract IP from address string without allocation
#[allow(dead_code)]
pub fn extract_ip(address: &str) -> &str {
    address.split(':').next().unwrap_or(address)
}
