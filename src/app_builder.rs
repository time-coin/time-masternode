use tracing::info;

/// Helper to calculate appropriate cache size based on available memory
pub fn calculate_cache_size() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};

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

/// Helper to open a sled database with standard configuration
pub fn open_sled_database(
    base_path: &str,
    name: &str,
    cache_size: u64,
) -> Result<sled::Db, sled::Error> {
    let path = format!("{}/{}", base_path, name);
    sled::Config::new()
        .path(&path)
        .cache_capacity(cache_size)
        .mode(sled::Mode::HighThroughput)
        .open()
}
