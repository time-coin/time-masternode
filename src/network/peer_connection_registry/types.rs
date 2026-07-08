//! Shared types and helpers for the peer connection registry.

use crate::network::message::NetworkMessage;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

pub(super) type PendingPingMap = HashMap<String, Vec<(u64, std::time::Instant)>>;

/// Channel-based writer: sends pre-serialized frame bytes to a dedicated I/O task.
/// This avoids `tokio::io::split()` on TLS streams, which causes frame corruption
/// due to shared internal mutex and waker issues.
pub type PeerWriterTx = mpsc::UnboundedSender<Vec<u8>>;
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
