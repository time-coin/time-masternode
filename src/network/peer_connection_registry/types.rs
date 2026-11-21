//! Shared types and helpers for the peer connection registry.

use crate::network::message::NetworkMessage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

pub(super) type PendingPingMap = HashMap<String, Vec<(u64, std::time::Instant)>>;

/// Max bytes that may sit in one peer's outbound queue. A peer that stops
/// reading (zero TCP window) while requesting large responses (GetUTXOSet,
/// GetBlockRange) would otherwise grow this queue until the node OOMs. Once the
/// cap is hit the writer is marked overflowed: further sends fail, the I/O task
/// exits and the connection is torn down. 64 MiB is far above any legitimate
/// burst (a full UTXO-set transfer on mainnet is a few MiB).
pub const MAX_PEER_WRITE_QUEUE_BYTES: usize = 64 * 1024 * 1024;

/// Channel-based writer: sends pre-serialized frame bytes to a dedicated I/O task.
/// This avoids `tokio::io::split()` on TLS streams, which causes frame corruption
/// due to shared internal mutex and waker issues.
///
/// Wraps an unbounded channel with a byte-count cap (see
/// [`MAX_PEER_WRITE_QUEUE_BYTES`]) so `send` stays synchronous and non-blocking.
#[derive(Clone, Debug)]
pub struct PeerWriterTx {
    inner: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    overflowed: Arc<AtomicBool>,
}

impl PeerWriterTx {
    /// Queue a frame. Fails if the I/O task is gone or the queue cap was exceeded.
    pub fn send(&self, data: Vec<u8>) -> Result<(), mpsc::error::SendError<Vec<u8>>> {
        if self.overflowed.load(Ordering::Relaxed) {
            return Err(mpsc::error::SendError(data));
        }
        let len = data.len();
        let prev = self.queued.fetch_add(len, Ordering::Relaxed);
        if prev.saturating_add(len) > MAX_PEER_WRITE_QUEUE_BYTES {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            if !self.overflowed.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "🚫 Peer outbound queue exceeded {} MiB (peer not reading) — dropping connection",
                    MAX_PEER_WRITE_QUEUE_BYTES / (1024 * 1024)
                );
            }
            return Err(mpsc::error::SendError(data));
        }
        self.inner.send(data).map_err(|e| {
            self.queued.fetch_sub(len, Ordering::Relaxed);
            e
        })
    }

    /// True once the I/O task has exited or the queue overflowed.
    pub fn is_closed(&self) -> bool {
        self.overflowed.load(Ordering::Relaxed) || self.inner.is_closed()
    }
}

/// Receiving half used by the per-connection writer task.
pub struct PeerWriterRx {
    inner: mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    overflowed: Arc<AtomicBool>,
}

impl PeerWriterRx {
    /// Next frame to write, or `None` when all senders are gone or the queue
    /// overflowed (the writer task should then exit, closing the stream).
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        if self.overflowed.load(Ordering::Relaxed) {
            return None;
        }
        let data = self.inner.recv().await?;
        self.queued.fetch_sub(data.len(), Ordering::Relaxed);
        if self.overflowed.load(Ordering::Relaxed) {
            return None;
        }
        Some(data)
    }
}

/// Create a byte-capped writer channel for one peer connection.
pub fn peer_writer_channel() -> (PeerWriterTx, PeerWriterRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    let queued = Arc::new(AtomicUsize::new(0));
    let overflowed = Arc::new(AtomicBool::new(false));
    (
        PeerWriterTx {
            inner: tx,
            queued: queued.clone(),
            overflowed: overflowed.clone(),
        },
        PeerWriterRx {
            inner: rx,
            queued,
            overflowed,
        },
    )
}
pub(super) type ResponseSender = oneshot::Sender<NetworkMessage>;
pub(super) type ChainTip = (u64, [u8; 32]); // (height, block_hash)

pub(super) fn extract_ip(addr: &str) -> &str {
    addr.split(':').next().unwrap_or(addr)
}

/// Information about an incompatible peer
/// (marked_timestamp, incompatibility_reason, is_permanent)
pub(super) type IncompatiblePeerInfo = (Instant, String, bool);

/// Type alias for shared writer channel that can be cloned and registered
pub type SharedPeerWriter = PeerWriterTx;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writer_queue_cap_overflows_and_closes() {
        let (tx, mut rx) = peer_writer_channel();
        let chunk = vec![0u8; 8 * 1024 * 1024];
        for _ in 0..(MAX_PEER_WRITE_QUEUE_BYTES / chunk.len()) {
            tx.send(chunk.clone()).expect("within cap");
        }
        assert!(!tx.is_closed());
        // One more byte past the cap overflows the writer.
        assert!(tx.send(vec![0u8; 1]).is_err());
        assert!(tx.is_closed());
        // Writer task sees the overflow and exits.
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn writer_queue_drains_and_accounts_bytes() {
        let (tx, mut rx) = peer_writer_channel();
        let big = vec![0u8; MAX_PEER_WRITE_QUEUE_BYTES - 10];
        tx.send(big).unwrap();
        assert_eq!(
            rx.recv().await.unwrap().len(),
            MAX_PEER_WRITE_QUEUE_BYTES - 10
        );
        // After draining, the full budget is available again.
        tx.send(vec![0u8; MAX_PEER_WRITE_QUEUE_BYTES]).unwrap();
        assert!(!tx.is_closed());
    }
}
