//! Shutdown manager for graceful application termination.
//!
//! Note: This module appears as "dead code" in library checks because it's
//! only used by the binary (main.rs). The ShutdownManager is instantiated
//! and used in main() for coordinating graceful shutdown of all tasks.

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Manages graceful shutdown of the application
pub struct ShutdownManager {
    /// Token to signal shutdown to all tasks
    cancel_token: CancellationToken,
    /// Handles to all spawned tasks
    task_handles: Vec<JoinHandle<()>>,
}

impl ShutdownManager {
    pub fn new() -> Self {
        Self {
            cancel_token: CancellationToken::new(),
            task_handles: Vec::new(),
        }
    }

    /// Get a clone of the cancellation token for spawning tasks
    pub fn token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Register a task handle for shutdown coordination
    pub fn register_task(&mut self, handle: JoinHandle<()>) {
        self.task_handles.push(handle);
    }

    /// Wait for ctrl+c and gracefully shut down all tasks
    pub async fn wait_for_shutdown(mut self) {
        // Listen for ctrl+c
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!("Failed to listen for shutdown signal: {}", e);
            return;
        }

        tracing::info!("🛑 Shutdown signal received");

        // Signal all tasks to stop
        self.cancel_token.cancel();

        // Wait for all tasks to complete with a timeout
        let timeout = tokio::time::Duration::from_secs(10);
        let shutdown_tasks = std::pin::pin!(async {
            for handle in self.task_handles.drain(..) {
                let _ = handle.await;
            }
        });

        match tokio::time::timeout(timeout, shutdown_tasks).await {
            Ok(_) => {
                tracing::info!("✓ All tasks shut down gracefully");
            }
            Err(_) => {
                tracing::warn!("⏱️  Shutdown timeout: Some tasks did not complete");
            }
        }
    }
}

impl Default for ShutdownManager {
    fn default() -> Self {
        Self::new()
    }
}
