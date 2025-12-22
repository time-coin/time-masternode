use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("Storage initialization error: {0}")]
    StorageInit(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Consensus error: {0}")]
    Consensus(String),

    #[error("Time sync failed: system clock off by {offset_seconds}s (max: {max_offset}s)")]
    TimeSyncFailed {
        offset_seconds: i64,
        max_offset: i64,
    },

    #[error("Block production error: {0}")]
    BlockProduction(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Task join error: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),

    #[error("Initialization error: {0}")]
    Initialization(String),
}

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Failed to open {name} database: {source}")]
    DatabaseOpen {
        name: String,
        #[source]
        source: sled::Error,
    },

    #[error("Database operation failed: {0}")]
    DatabaseOp(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}
